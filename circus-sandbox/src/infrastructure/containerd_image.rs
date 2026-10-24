//! Local image resolution and unpacking through containerd's content, diff,
//! image, and snapshot services.
//!
//! Nothing is pulled: every blob must already be in the content store. Unpack
//! is performed only when the image's committed chain snapshots are missing,
//! using the local layer descriptors and the configured snapshotter.

use std::collections::HashMap;

use containerd_client::services::v1::{
    GetImageRequest, ReadContentRequest,
    content_client::ContentClient,
    diff_client::DiffClient,
    images_client::ImagesClient,
    snapshots::{
        CommitSnapshotRequest, MountsRequest, PrepareSnapshotRequest, RemoveSnapshotRequest,
        StatSnapshotRequest, snapshots_client::SnapshotsClient,
    },
};
use containerd_client::tonic::Request;
use containerd_client::tonic::transport::Channel;
use containerd_client::types::{Descriptor, Mount};
use containerd_client::with_namespace;

use super::containerd_grpc::map_status;
use super::engine::{EngineError, EngineErrorKind, file_error};

const MAX_METADATA_BYTES: usize = 8 * 1024 * 1024;
const UNPACK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
const UNPACK_POLL_ATTEMPTS: usize = 600;
const INDEX_MEDIA_TYPES: [&str; 2] = [
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
];

/// One layer descriptor together with its containerd chain identity.
pub(crate) struct ChainLayer {
    pub chain_id: String,
    pub descriptor: Descriptor,
}

pub(crate) struct ImageRootfs {
    pub chain_id: String,
    pub layers: Vec<ChainLayer>,
}

// One unpack at a time inside this process: two sandboxes created
// concurrently from the same unpacked image must not apply a layer twice.
static UNPACK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Verifies that an image record exists in the namespace without pulling.
///
/// Familiar short references are resolved the way containerd's own clients do:
/// `python:3.12-slim` is looked up as `docker.io/library/python:3.12-slim`
/// after the literal name. Nothing is normalized on the daemon.
pub(crate) async fn image_exists(
    channel: &Channel,
    namespace: &str,
    image: &str,
) -> Result<(), EngineError> {
    get_image(channel, namespace, image).await.map(|_| ())
}

async fn get_image(
    channel: &Channel,
    namespace: &str,
    image: &str,
) -> Result<containerd_client::services::v1::Image, EngineError> {
    let mut client = ImagesClient::new(channel.clone());
    let mut last = None;
    for candidate in image_candidates(image) {
        let request = with_namespace!(GetImageRequest { name: candidate }, namespace);
        match client.get(request).await {
            Ok(response) => {
                return response.into_inner().image.ok_or_else(|| {
                    file_error(EngineErrorKind::Other, "image record has no target")
                });
            }
            Err(status) if status.code() == containerd_client::tonic::Code::NotFound => {
                last = Some(map_status(status));
            }
            Err(status) => return Err(map_status(status)),
        }
    }
    Err(last.unwrap_or_else(|| file_error(EngineErrorKind::NotFound, "image reference is empty")))
}

/// Candidate containerd names for one requested reference.
pub(crate) fn image_candidates(image: &str) -> Vec<String> {
    let mut candidates = vec![image.to_string()];
    let mut parts = image.splitn(2, '/');
    let first = parts.next().unwrap_or_default();
    let rest = parts.next();
    let normalized = match rest {
        None => format!("docker.io/library/{image}"),
        Some(_) if first.contains('.') || first.contains(':') || first == "localhost" => {
            return candidates;
        }
        Some(_) => format!("docker.io/{image}"),
    };
    candidates.push(normalized);
    candidates
}

/// Resolves the image's rootfs chain and ensures every committed snapshot
/// exists, unpacking local layers only when they do not.
pub(crate) async fn ensure_rootfs(
    channel: &Channel,
    namespace: &str,
    snapshotter: &str,
    image: &str,
) -> Result<String, EngineError> {
    let _guard = UNPACK_LOCK.lock().await;
    let rootfs = resolve_rootfs(channel, namespace, image).await?;
    if snapshot_committed(channel, namespace, snapshotter, &rootfs.chain_id).await? {
        return Ok(rootfs.chain_id);
    }
    let mut parent = String::new();
    for layer in &rootfs.layers {
        if !snapshot_committed(channel, namespace, snapshotter, &layer.chain_id).await? {
            unpack_layer(
                channel,
                namespace,
                snapshotter,
                &layer.chain_id,
                &parent,
                &layer.descriptor,
            )
            .await?;
        }
        parent = layer.chain_id.clone();
    }
    if !snapshot_committed(channel, namespace, snapshotter, &rootfs.chain_id).await? {
        return Err(file_error(
            EngineErrorKind::Other,
            "image unpack completed without the final committed snapshot",
        ));
    }
    Ok(rootfs.chain_id)
}

async fn resolve_rootfs(
    channel: &Channel,
    namespace: &str,
    image: &str,
) -> Result<ImageRootfs, EngineError> {
    let image = get_image(channel, namespace, image).await?;
    let target = image.target.ok_or_else(|| {
        file_error(
            EngineErrorKind::Other,
            "image record has no target descriptor",
        )
    })?;
    let manifest = if INDEX_MEDIA_TYPES.contains(&target.media_type.as_str()) {
        index_manifest(channel, namespace, &target).await?
    } else {
        target
    };
    let manifest_json = read_content(channel, namespace, &manifest).await?;
    let parsed = parse_manifest(&manifest_json)?;
    let config_json = read_content(channel, namespace, &parsed.config).await?;
    let diff_ids = parse_diff_ids(&config_json)?;
    if diff_ids.is_empty() {
        return Err(file_error(
            EngineErrorKind::Other,
            "image config has no rootfs layers",
        ));
    }
    if diff_ids.len() != parsed.layers.len() {
        return Err(file_error(
            EngineErrorKind::Other,
            "image config diff_ids and manifest layers disagree",
        ));
    }
    let chain = chain_ids(&diff_ids);
    let layers = chain
        .into_iter()
        .zip(parsed.layers)
        .map(|(chain_id, descriptor)| ChainLayer {
            chain_id,
            descriptor,
        })
        .collect();
    Ok(ImageRootfs {
        chain_id: chain_id_for(&diff_ids),
        layers,
    })
}

async fn index_manifest(
    channel: &Channel,
    namespace: &str,
    target: &Descriptor,
) -> Result<Descriptor, EngineError> {
    let index = read_content(channel, namespace, target).await?;
    let value: serde_json::Value = serde_json::from_slice(&index).map_err(|err| {
        file_error(
            EngineErrorKind::Other,
            format!("image index is not JSON: {err}"),
        )
    })?;
    let manifests = value
        .get("manifests")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| file_error(EngineErrorKind::Other, "image index has no manifests"))?;
    let host = host_architecture();
    let selected = manifests
        .iter()
        .find(|entry| platform_matches(entry, "linux", host))
        .ok_or_else(|| {
            file_error(
                EngineErrorKind::Other,
                format!("image index has no linux/{host} manifest"),
            )
        })?;
    descriptor_from_value(selected)
}

fn platform_matches(entry: &serde_json::Value, os: &str, architecture: &str) -> bool {
    let platform = entry.get("platform");
    let matches = |field: &str, expected: &str| {
        platform
            .and_then(|platform| platform.get(field))
            .and_then(serde_json::Value::as_str)
            == Some(expected)
    };
    matches("os", os) && matches("architecture", architecture)
}

fn descriptor_from_value(value: &serde_json::Value) -> Result<Descriptor, EngineError> {
    let media_type = value
        .get("mediaType")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| file_error(EngineErrorKind::Other, "descriptor has no mediaType"))?;
    let digest = value
        .get("digest")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| file_error(EngineErrorKind::Other, "descriptor has no digest"))?;
    let size = value
        .get("size")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_default();
    Ok(Descriptor {
        media_type: media_type.to_string(),
        digest: digest.to_string(),
        size,
        annotations: HashMap::new(),
    })
}

struct Manifest {
    config: Descriptor,
    layers: Vec<Descriptor>,
}

fn parse_manifest(bytes: &[u8]) -> Result<Manifest, EngineError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|err| {
        file_error(
            EngineErrorKind::Other,
            format!("image manifest is not JSON: {err}"),
        )
    })?;
    let config = value
        .get("config")
        .ok_or_else(|| file_error(EngineErrorKind::Other, "manifest has no config"))?;
    let layers = value
        .get("layers")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| file_error(EngineErrorKind::Other, "manifest has no layers"))?;
    Ok(Manifest {
        config: descriptor_from_value(config)?,
        layers: layers
            .iter()
            .map(descriptor_from_value)
            .collect::<Result<_, _>>()?,
    })
}

fn parse_diff_ids(bytes: &[u8]) -> Result<Vec<String>, EngineError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|err| {
        file_error(
            EngineErrorKind::Other,
            format!("image config is not JSON: {err}"),
        )
    })?;
    let ids = value
        .pointer("/rootfs/diff_ids")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| file_error(EngineErrorKind::Other, "image config has no diff_ids"))?;
    ids.iter()
        .map(|id| {
            id.as_str()
                .map(str::to_string)
                .ok_or_else(|| file_error(EngineErrorKind::Other, "image diff_id is not a string"))
        })
        .collect()
}

/// containerd chain identity: each step hashes the previous chain id and the
/// next diff id; the final chain id names the committed rootfs snapshot.
fn chain_ids(diff_ids: &[String]) -> Vec<String> {
    let mut chain: Vec<String> = Vec::with_capacity(diff_ids.len());
    for diff_id in diff_ids {
        match chain.last() {
            None => chain.push(diff_id.clone()),
            Some(previous) => chain.push(sha256_hex(format!("{previous} {diff_id}"))),
        }
    }
    chain
}

fn chain_id_for(diff_ids: &[String]) -> String {
    chain_ids(diff_ids).pop().unwrap_or_default()
}

fn sha256_hex(input: String) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex(&Sha256::digest(input.as_bytes())))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

fn host_architecture() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "x86" => "386",
        "aarch64" => "arm64",
        "arm" => "arm",
        "powerpc64" => "ppc64le",
        "s390x" => "s390x",
        "riscv64" => "riscv64",
        other => other,
    }
}

async fn read_content(
    channel: &Channel,
    namespace: &str,
    descriptor: &Descriptor,
) -> Result<Vec<u8>, EngineError> {
    let mut client = ContentClient::new(channel.clone());
    let request = with_namespace!(
        ReadContentRequest {
            digest: descriptor.digest.clone(),
            offset: 0,
            size: descriptor.size,
        },
        namespace
    );
    let mut stream = client.read(request).await.map_err(map_status)?.into_inner();
    let mut bytes = Vec::new();
    while let Some(message) = stream.message().await.map_err(map_status)? {
        if bytes.len().saturating_add(message.data.len()) > MAX_METADATA_BYTES {
            return Err(file_error(
                EngineErrorKind::Limit,
                "image metadata exceeds the 8 MiB bound",
            ));
        }
        bytes.extend_from_slice(&message.data);
    }
    Ok(bytes)
}

async fn snapshot_committed(
    channel: &Channel,
    namespace: &str,
    snapshotter: &str,
    key: &str,
) -> Result<bool, EngineError> {
    let mut client = SnapshotsClient::new(channel.clone());
    let request = with_namespace!(
        StatSnapshotRequest {
            snapshotter: snapshotter.to_string(),
            key: key.to_string(),
        },
        namespace
    );
    match client.stat(request).await {
        Ok(_) => Ok(true),
        Err(status) if status.code() == containerd_client::tonic::Code::NotFound => Ok(false),
        Err(status) => Err(map_status(status)),
    }
}

async fn unpack_layer(
    channel: &Channel,
    namespace: &str,
    snapshotter: &str,
    key: &str,
    parent: &str,
    descriptor: &Descriptor,
) -> Result<(), EngineError> {
    let mounts = prepare_snapshot(channel, namespace, snapshotter, key, parent).await?;
    let applied = apply_diff(channel, namespace, descriptor, &mounts).await;
    if let Err(error) = applied {
        remove_snapshot_best_effort(channel, namespace, snapshotter, key).await;
        return Err(error);
    }
    let mut client = SnapshotsClient::new(channel.clone());
    let request = with_namespace!(
        CommitSnapshotRequest {
            snapshotter: snapshotter.to_string(),
            name: key.to_string(),
            key: key.to_string(),
            // Referencing the layer content keeps the committed snapshot alive
            // under containerd's garbage collector; an unlabelled snapshot can
            // be reclaimed while another sandbox is mounting it.
            labels: HashMap::from([(
                "containerd.io/gc.ref.content.0".to_string(),
                descriptor.digest.clone(),
            )]),
        },
        namespace
    );
    if let Err(status) = client.commit(request).await {
        let error = map_status(status);
        remove_snapshot_best_effort(channel, namespace, snapshotter, key).await;
        return Err(error);
    }
    Ok(())
}

async fn prepare_snapshot(
    channel: &Channel,
    namespace: &str,
    snapshotter: &str,
    key: &str,
    parent: &str,
) -> Result<Vec<Mount>, EngineError> {
    let mut client = SnapshotsClient::new(channel.clone());
    let request = with_namespace!(
        PrepareSnapshotRequest {
            snapshotter: snapshotter.to_string(),
            key: key.to_string(),
            parent: parent.to_string(),
            labels: HashMap::new(),
        },
        namespace
    );
    match client.prepare(request).await {
        Ok(response) => Ok(response.into_inner().mounts),
        Err(status) if status.code() == containerd_client::tonic::Code::AlreadyExists => {
            // Another process is unpacking this chain; wait for its commit and
            // use the committed snapshot's mounts.
            for _ in 0..UNPACK_POLL_ATTEMPTS {
                if snapshot_committed(channel, namespace, snapshotter, key).await? {
                    return snapshot_mounts(channel, namespace, snapshotter, key).await;
                }
                tokio::time::sleep(UNPACK_POLL_INTERVAL).await;
            }
            Err(map_status(status))
        }
        Err(status) => Err(map_status(status)),
    }
}

async fn snapshot_mounts(
    channel: &Channel,
    namespace: &str,
    snapshotter: &str,
    key: &str,
) -> Result<Vec<Mount>, EngineError> {
    let mut client = SnapshotsClient::new(channel.clone());
    let request = with_namespace!(
        MountsRequest {
            snapshotter: snapshotter.to_string(),
            key: key.to_string(),
        },
        namespace
    );
    Ok(client
        .mounts(request)
        .await
        .map_err(map_status)?
        .into_inner()
        .mounts)
}

async fn apply_diff(
    channel: &Channel,
    namespace: &str,
    descriptor: &Descriptor,
    mounts: &[Mount],
) -> Result<(), EngineError> {
    let mut client = DiffClient::new(channel.clone());
    let request = with_namespace!(
        containerd_client::services::v1::ApplyRequest {
            diff: Some(descriptor.clone()),
            mounts: mounts.to_vec(),
            payloads: HashMap::new(),
            sync_fs: false,
        },
        namespace
    );
    client.apply(request).await.map_err(map_status)?;
    Ok(())
}

async fn remove_snapshot_best_effort(
    channel: &Channel,
    namespace: &str,
    snapshotter: &str,
    key: &str,
) {
    let mut client = SnapshotsClient::new(channel.clone());
    let request = with_namespace!(
        RemoveSnapshotRequest {
            snapshotter: snapshotter.to_string(),
            key: key.to_string(),
        },
        namespace
    );
    let _ = client.remove(request).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    // Known containerd chain ids for python:3.12-slim linux/amd64, read from
    // the live daemon's committed snapshots. The algorithm must match exactly.
    const DIFF_IDS: [&str; 4] = [
        "sha256:a70f6272827f64e1427d048ced77491aab5d64ab95725e56920802ff391197d7",
        "sha256:33d3a946838548b1885b1068b960a88e41537b6996342c8fc2c767a6879e62f4",
        "sha256:500ecb68a701ad0437ae759f35178f2ddbc48e10a0bc6dc345548054f737df6a",
        "sha256:93881a54803b57e0039f40fd78de98b41b69b55d025c431544788fd84f0904cd",
    ];

    #[test]
    fn chain_ids_match_containerd_unpack() {
        let diff_ids = DIFF_IDS.map(str::to_string).to_vec();
        let chain = chain_ids(&diff_ids);
        assert_eq!(chain[0], DIFF_IDS[0]);
        assert_eq!(
            chain[1],
            "sha256:af995e0458453007a40b917922dc59b8834ba36fb1564fa85ba6f9d6795e8629"
        );
        assert_eq!(
            chain[2],
            "sha256:24eb450e9d3b775f6a1200544ef8f5da86b0d3af323e04a803f011cb246c9915"
        );
        assert_eq!(
            chain[3],
            "sha256:241d372eec8f05a802bb5ca99fba52dd93480b904cb0ca4f591e4f2e27ffd6da"
        );
    }

    #[test]
    fn sha256_hex_is_lowercase_64_digits() {
        let digest = sha256_hex("abc".to_string());
        assert_eq!(
            digest,
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn manifest_parsing_keeps_layer_order() {
        let manifest = br#"{
            "config": {"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:a","size":2},
            "layers": [
                {"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"sha256:b","size":3},
                {"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"sha256:c","size":4}
            ]
        }"#;
        let parsed = parse_manifest(manifest).unwrap();
        assert_eq!(parsed.config.digest, "sha256:a");
        assert_eq!(parsed.layers.len(), 2);
        assert_eq!(parsed.layers[1].digest, "sha256:c");
        assert_eq!(parsed.layers[1].size, 4);
    }

    #[test]
    fn diff_ids_require_rootfs_array() {
        assert!(parse_diff_ids(br#"{"rootfs":{"diff_ids":["sha256:a"]}}"#).is_ok());
        assert!(parse_diff_ids(br#"{"rootfs":{}}"#).is_err());
        assert!(parse_diff_ids(b"not json").is_err());
    }

    #[test]
    fn familiar_references_are_looked_up_at_docker_hub() {
        assert_eq!(
            image_candidates("python:3.12-slim"),
            vec![
                "python:3.12-slim".to_string(),
                "docker.io/library/python:3.12-slim".to_string(),
            ]
        );
        assert_eq!(
            image_candidates("library/python:3.12-slim"),
            vec![
                "library/python:3.12-slim".to_string(),
                "docker.io/library/python:3.12-slim".to_string(),
            ]
        );
        assert_eq!(
            image_candidates("docker.io/library/python:3.12-slim"),
            vec!["docker.io/library/python:3.12-slim".to_string()]
        );
        assert_eq!(
            image_candidates("localhost:5000/app:1"),
            vec!["localhost:5000/app:1".to_string()]
        );
    }
}
