use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::{
    CodeSnapshot, SnapshotBudget, SnapshotError, Workspace, abandon_failed_restore,
    require_portable,
};

pub(super) fn from_workspace_archive(
    mut file: std::fs::File,
    expected_root: &str,
    max_file_bytes: usize,
    budget: SnapshotBudget,
) -> Result<CodeSnapshot, SnapshotError> {
    require_portable(Path::new(expected_root))?;
    if expected_root.contains('/') {
        return Err(invalid("expected root must be one component"));
    }
    validate_metadata(&mut file, max_file_bytes)?;
    let mut workspace = Workspace::new(max_file_bytes)?;
    match import(&workspace, file, expected_root, max_file_bytes, budget) {
        Ok(snapshot) => match workspace.cleanup() {
            Ok(()) => Ok(snapshot),
            Err(cleanup) => Err(SnapshotError::ArchiveCleanup {
                archive: Box::new(workspace),
                cleanup,
            }),
        },
        Err(err) => Err(abandon_failed_restore(workspace, err)),
    }
}

fn import(
    workspace: &Workspace,
    file: std::fs::File,
    root: &str,
    max_file_bytes: usize,
    budget: SnapshotBudget,
) -> Result<CodeSnapshot, SnapshotError> {
    let mut archive = tar::Archive::new(file);
    let mut seen = HashSet::<PathBuf>::new();
    let mut root_seen = false;
    let mut total = 0usize;
    let mut entries = 0usize;
    let mut directories = HashSet::<PathBuf>::new();
    let mut bytes = Vec::new();
    // The preceding raw scan bounds extension allocation before tar applies it.
    for item in archive.entries().map_err(|e| invalid(e.to_string()))? {
        let mut entry = item.map_err(|e| invalid(e.to_string()))?;
        let ty = entry.header().entry_type();
        if !ty.is_dir() && !ty.is_file() {
            return Err(invalid("unsupported archive entry type or metadata"));
        }
        if entry.header().link_name_bytes().is_some() {
            return Err(invalid("unexpected link metadata"));
        }
        let raw_path = entry.path_bytes().into_owned();
        let text = std::str::from_utf8(&raw_path).map_err(|_| invalid("non-UTF-8 archive path"))?;
        let text = if ty.is_dir() {
            text.strip_suffix('/').unwrap_or(text)
        } else {
            text
        };
        if text
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(invalid("non-normal archive path"));
        }
        require_portable(Path::new(text))?;
        let (entry_root, relative) = text.split_once('/').unwrap_or((text, ""));
        if entry_root != root {
            return Err(invalid("entry outside expected workspace root"));
        }
        let size = usize::try_from(entry.size())
            .map_err(|_| invalid("file size exceeds platform limit"))?;
        if ty.is_dir() && size != 0 {
            return Err(invalid("directory has payload"));
        }
        if relative.is_empty() {
            if !ty.is_dir() || root_seen {
                return Err(invalid("invalid or duplicate root entry"));
            }
            root_seen = true;
            continue;
        }
        let rel = Path::new(relative);
        require_portable(rel)?;
        if !seen.insert(rel.to_owned()) {
            return Err(invalid("duplicate archive path"));
        }
        entries = entries
            .checked_add(1)
            .ok_or_else(|| invalid("entry count overflow"))?;
        if entries > budget.max_entries() {
            return Err(SnapshotError::EntryLimit {
                limit: budget.max_entries(),
                actual: entries,
            });
        }
        if let Some(parent) = rel.parent()
            && !parent.as_os_str().is_empty()
            && !directories.contains(parent)
        {
            return Err(invalid("parent directory must precede its children"));
        }
        if ty.is_dir() {
            workspace.create_dir_all(rel)?;
            directories.insert(rel.to_owned());
            continue;
        }
        if size > max_file_bytes {
            return Err(super::WorkspaceError::LimitExceeded {
                limit: max_file_bytes,
                actual: size,
            }
            .into());
        }
        total = total
            .checked_add(size)
            .ok_or_else(|| invalid("payload length overflow"))?;
        if total > budget.max_total_bytes() {
            return Err(SnapshotError::ByteLimit {
                limit: budget.max_total_bytes(),
                actual: total,
            });
        }
        bytes.clear();
        bytes
            .try_reserve_exact(size)
            .map_err(super::reserve_error)?;
        (&mut entry)
            .take(size as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| invalid(e.to_string()))?;
        if bytes.len() != size {
            return Err(invalid("archive payload length mismatch"));
        }
        workspace.write_file(rel, &bytes)?;
    }
    if !root_seen {
        return Err(invalid("missing expected root directory"));
    }
    workspace.capture_code_snapshot(budget)
}

// Native Docker tar streams can contain PAX or GNU long path extensions. Limit
// each extension before the mature parser reads it into memory.
fn validate_metadata(file: &mut std::fs::File, max_file_bytes: usize) -> Result<(), SnapshotError> {
    const MAX_METADATA_BYTES: u64 = 1024 * 1024;
    let mut long_name_pending = false;
    let mut pax_pending = false;
    {
        let mut archive = tar::Archive::new(&mut *file);
        for item in archive
            .entries()
            .map_err(|err| invalid(err.to_string()))?
            .raw(true)
        {
            let mut entry = item.map_err(|err| invalid(err.to_string()))?;
            let kind = entry.header().entry_type().as_byte();
            match kind {
                b'0' | 0 => {
                    long_name_pending = false;
                    pax_pending = false;
                    if entry.size() > max_file_bytes as u64 {
                        return Err(super::WorkspaceError::LimitExceeded {
                            limit: max_file_bytes,
                            actual: usize::try_from(entry.size()).unwrap_or(usize::MAX),
                        }
                        .into());
                    }
                }
                b'5' => {
                    long_name_pending = false;
                    pax_pending = false;
                }
                b'L' | b'x' => {
                    // At most one extension of each kind can precede a real
                    // entry. This bounds the parser's live extension state,
                    // without limiting metadata across independent entries.
                    let pending = if kind == b'L' {
                        &mut long_name_pending
                    } else {
                        &mut pax_pending
                    };
                    if *pending {
                        return Err(invalid("repeated local archive extension"));
                    }
                    *pending = true;
                    if entry.size() > MAX_METADATA_BYTES {
                        return Err(invalid("archive metadata entry exceeds 1 MiB"));
                    }
                    if kind != b'L' {
                        let extensions = entry
                            .pax_extensions()
                            .map_err(|err| invalid(err.to_string()))?
                            .ok_or_else(|| invalid("missing PAX extension payload"))?;
                        for extension in extensions {
                            let extension = extension.map_err(|err| invalid(err.to_string()))?;
                            let key = extension.key().map_err(|err| invalid(err.to_string()))?;
                            if !matches!(
                                key,
                                "path"
                                    | "size"
                                    | "mtime"
                                    | "atime"
                                    | "ctime"
                                    | "uid"
                                    | "gid"
                                    | "uname"
                                    | "gname"
                            ) && !key.starts_with("SCHILY.xattr.")
                            {
                                return Err(invalid("unsupported PAX metadata"));
                            }
                        }
                    }
                }
                _ => return Err(invalid("unsupported archive entry type or metadata")),
            }
        }
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|err| invalid(err.to_string()))?;
    Ok(())
}

fn invalid(reason: impl Into<String>) -> SnapshotError {
    SnapshotError::InvalidArchive {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests;
