use std::io::{BufRead, BufReader, BufWriter, Write as _};
use std::path::Path;

use sha2::{Digest as _, Sha256};

use crate::util::{PrivateEntryKind, PrivateRoot, PrivateRootReader, PrivateTreeEntry};

use super::error::SessionMigrationError;

const TREE_DIGEST_DOMAIN: &[u8] = b"norn-session-tree-sha256\0";

/// Hash sorted topology and every file byte without loading whole files.
pub(super) fn digest_tree(
    reader: &PrivateRootReader,
    tree: &[PrivateTreeEntry],
) -> Result<String, SessionMigrationError> {
    let mut hasher = Sha256::new();
    hasher.update(TREE_DIGEST_DOMAIN);
    let mut previous = None;
    for entry in tree {
        if previous.as_ref().is_some_and(|path| path >= &entry.path) {
            return Err(SessionMigrationError::UnrepresentableSource {
                reason: "observational tree entries are not uniquely sorted".to_owned(),
            });
        }
        previous = Some(entry.path.clone());
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStrExt as _;

            entry.path.as_os_str().as_bytes()
        };
        #[cfg(not(unix))]
        let path = entry.path.to_str().map(str::as_bytes).ok_or_else(|| {
            SessionMigrationError::UnrepresentableSource {
                reason: format!("source path is not valid UTF-8: {}", entry.path.display()),
            }
        })?;
        let path_length = u64::try_from(path.len()).map_err(|error| {
            SessionMigrationError::UnrepresentableSource {
                reason: format!("tree path length is not representable: {error}"),
            }
        })?;
        match entry.kind {
            PrivateEntryKind::Directory => {
                hasher.update(b"D");
                hasher.update(path_length.to_be_bytes());
                hasher.update(path);
            }
            PrivateEntryKind::File => {
                let expected =
                    entry
                        .length
                        .ok_or_else(|| SessionMigrationError::UnrepresentableSource {
                            reason: format!(
                                "observational file entry lacks a length: {}",
                                entry.path.display()
                            ),
                        })?;
                hasher.update(b"F");
                hasher.update(path_length.to_be_bytes());
                hasher.update(path);
                hasher.update(expected.to_be_bytes());
                let file = reader
                    .open_file(&entry.path)
                    .map_err(|error| SessionMigrationError::observation(&entry.path, error))?;
                let actual = hash_file_into(BufReader::new(file), &mut hasher, &entry.path)?;
                if actual != expected {
                    return Err(SessionMigrationError::Observation {
                        path: entry.path.clone(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!(
                                "file length changed during observation: expected {expected}, read {actual}"
                            ),
                        ),
                    });
                }
            }
            PrivateEntryKind::Other => {
                return Err(SessionMigrationError::UnrepresentableSource {
                    reason: format!(
                        "observational tree contains unsupported entry {}",
                        entry.path.display()
                    ),
                });
            }
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Stream a complete observed tree into a newly-created private subtree.
pub(super) fn copy_tree(
    source: &PrivateRootReader,
    tree: &[PrivateTreeEntry],
    destination: &PrivateRoot,
    prefix: &Path,
) -> Result<(), SessionMigrationError> {
    destination
        .create_dir_all(prefix)
        .map_err(|error| SessionMigrationError::mutation("creating backup root", prefix, error))?;
    let mut directories = vec![prefix.to_path_buf()];
    for entry in tree {
        let target = prefix.join(&entry.path);
        match entry.kind {
            PrivateEntryKind::Directory => {
                destination.create_dir_all(&target).map_err(|error| {
                    SessionMigrationError::mutation("creating backup directory", &target, error)
                })?;
                directories.push(target);
            }
            PrivateEntryKind::File => {
                if let Some(parent) = target.parent() {
                    destination.create_dir_all(parent).map_err(|error| {
                        SessionMigrationError::mutation(
                            "creating backup file parent",
                            parent,
                            error,
                        )
                    })?;
                }
                copy_file(source, entry, destination, &target)?;
            }
            PrivateEntryKind::Other => {
                return Err(SessionMigrationError::UnrepresentableSource {
                    reason: format!(
                        "observational tree contains unsupported entry {}",
                        entry.path.display()
                    ),
                });
            }
        }
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    directories.dedup();
    for directory in directories {
        destination.sync_dir(&directory).map_err(|error| {
            SessionMigrationError::mutation("synchronizing backup directory", &directory, error)
        })?;
    }
    Ok(())
}

pub(super) fn copy_one_file(
    source: &PrivateRootReader,
    source_path: &Path,
    expected_length: u64,
    destination: &PrivateRoot,
    destination_path: &Path,
) -> Result<(), SessionMigrationError> {
    let entry = PrivateTreeEntry {
        path: source_path.to_path_buf(),
        kind: PrivateEntryKind::File,
        length: Some(expected_length),
        mode: 0,
    };
    if let Some(parent) = destination_path.parent() {
        destination.create_dir_all(parent).map_err(|error| {
            SessionMigrationError::mutation("creating migrated file parent", parent, error)
        })?;
    }
    copy_file(source, &entry, destination, destination_path)
}

fn copy_file(
    source: &PrivateRootReader,
    entry: &PrivateTreeEntry,
    destination: &PrivateRoot,
    target: &Path,
) -> Result<(), SessionMigrationError> {
    let expected = entry
        .length
        .ok_or_else(|| SessionMigrationError::UnrepresentableSource {
            reason: format!("source file lacks a length: {}", entry.path.display()),
        })?;
    let mut input = source
        .open_file(&entry.path)
        .map_err(|error| SessionMigrationError::observation(&entry.path, error))?;
    let output = destination.create_new(target).map_err(|error| {
        SessionMigrationError::mutation("creating migrated file", target, error)
    })?;
    let mut output = BufWriter::new(output);
    let actual = std::io::copy(&mut input, &mut output)
        .map_err(|error| SessionMigrationError::mutation("copying migrated file", target, error))?;
    if actual != expected {
        return Err(SessionMigrationError::Observation {
            path: entry.path.clone(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "source file length changed during copy: expected {expected}, copied {actual}"
                ),
            ),
        });
    }
    output.flush().map_err(|error| {
        SessionMigrationError::mutation("flushing migrated file", target, error)
    })?;
    let file = output
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)
        .map_err(|error| {
            SessionMigrationError::mutation("finishing migrated file", target, error)
        })?;
    file.sync_all().map_err(|error| {
        SessionMigrationError::mutation("synchronizing migrated file", target, error)
    })?;
    if let Some(parent) = target.parent() {
        destination.sync_dir(parent).map_err(|error| {
            SessionMigrationError::mutation("synchronizing migrated file parent", parent, error)
        })?;
    }
    Ok(())
}

fn hash_file_into<R: BufRead>(
    mut reader: R,
    hasher: &mut Sha256,
    path: &Path,
) -> Result<u64, SessionMigrationError> {
    let mut total = 0_u64;
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| SessionMigrationError::observation(path, error))?;
        if available.is_empty() {
            break;
        }
        hasher.update(available);
        total = total
            .checked_add(u64::try_from(available.len()).map_err(|error| {
                SessionMigrationError::UnrepresentableSource {
                    reason: format!("read length is not representable: {error}"),
                }
            })?)
            .ok_or_else(|| SessionMigrationError::UnrepresentableSource {
                reason: format!(
                    "file length exceeds digest representation: {}",
                    path.display()
                ),
            })?;
        let consumed = available.len();
        reader.consume(consumed);
    }
    Ok(total)
}
