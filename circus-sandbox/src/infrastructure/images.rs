//! Image inspection through the same negotiated, explicitly selected Engine client.

use super::{Engine, EngineError, map_err};

pub(crate) struct ImageInfo {
    pub id: String,
    pub repo_digests: Vec<String>,
    pub os: String,
    pub architecture: String,
}

impl Engine {
    /// Inspects one local image. Never contacts a registry or pulls.
    pub(crate) async fn inspect_image(&self, reference: &str) -> Result<ImageInfo, EngineError> {
        let image = self
            .docker
            .inspect_image(reference)
            .await
            .map_err(map_err)?;
        Ok(ImageInfo {
            id: required(image.id, "id")?,
            // Locally built/imported images need not have any registry digest.
            repo_digests: image.repo_digests.unwrap_or_default(),
            os: required(image.os, "os")?,
            architecture: required(image.architecture, "architecture")?,
        })
    }
}

fn required(value: Option<String>, field: &str) -> Result<String, EngineError> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| EngineError::new(format!("image inspection omitted required {field}")))
}
