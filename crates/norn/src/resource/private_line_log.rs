//! Narrow private storage for append-only user-level text history.

use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use crate::util::{PrivateFileIdentity, PrivateRoot, validate_private_component};

/// One private regular file that can be read and appended one line at a time.
///
/// Each transaction reopens the private parent under weighted descriptor
/// admission. Every file open is descriptor-relative, refuses links and
/// non-regular files, heals mode to `0600`, and verifies the file identity
/// captured by the first successful read or append.
#[derive(Debug)]
pub struct PrivateLineLog {
    parent: PathBuf,
    relative: PathBuf,
    lock_relative: PathBuf,
    identity: Mutex<Option<PrivateFileIdentity>>,
}

impl PrivateLineLog {
    /// Bind an absolute file path to a private descriptor-pinned parent.
    ///
    /// The file is created lazily on the first append. Missing parent
    /// directories are created privately; relative paths and unsupported
    /// platforms fail closed.
    pub fn new(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "private line-log path must be absolute",
            ));
        }
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "private line-log path must have a parent directory",
            )
        })?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "private line-log path must have a UTF-8 file name",
                )
            })?;
        validate_private_component(name, "private line-log file name")?;
        let _descriptor_permit = acquire_private_fs_io()?;
        drop(PrivateRoot::create(parent)?);
        Ok(Self {
            parent: parent.to_path_buf(),
            relative: PathBuf::from(name),
            lock_relative: PathBuf::from(format!("{name}.lock")),
            identity: Mutex::new(None),
        })
    }

    /// The diagnostic spelling of the bound file path.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.parent.join(&self.relative)
    }

    /// Read the complete UTF-8 text file.
    pub fn read_to_string(&self) -> io::Result<String> {
        let _descriptor_permit = acquire_private_fs_io()?;
        let root = PrivateRoot::open(&self.parent)?;
        let _lock = self.lock(&root)?;
        let mut contents = String::new();
        let mut file = root.open_read(&self.relative)?;
        self.verify_or_bind(&file)?;
        file.read_to_string(&mut contents)?;
        if !contents.is_empty() && !contents.ends_with('\n') {
            let complete_len = contents.rfind('\n').map_or(0, |position| position + 1);
            contents.truncate(complete_len);
        }
        Ok(contents)
    }

    /// Append one physical line.
    ///
    /// Newline-bearing input is rejected so callers cannot accidentally break
    /// the record boundary. This operation does not promise cross-process
    /// transactionality beyond the underlying append-only file semantics.
    pub fn append_line(&self, line: &str) -> io::Result<()> {
        if line.contains(['\r', '\n']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "private line-log records must not contain newlines",
            ));
        }
        let mut record = Vec::with_capacity(line.len() + 1);
        record.extend_from_slice(line.as_bytes());
        record.push(b'\n');
        let _descriptor_permit = acquire_private_fs_io()?;
        let root = PrivateRoot::open(&self.parent)?;
        let _lock = self.lock(&root)?;
        match root.open_read_append(&self.relative) {
            Ok(mut file) => {
                self.verify_or_bind(&file)?;
                truncate_incomplete_tail(&mut file)?;
                file.write_all(&record)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut file = root.open_append_create(&self.relative)?;
                self.verify_or_bind(&file)?;
                file.write_all(&record)
            }
            Err(error) => Err(error),
        }
    }

    fn lock(&self, root: &PrivateRoot) -> io::Result<File> {
        let lock = root.open_lock(&self.lock_relative)?;
        File::lock(&lock)?;
        Ok(lock)
    }

    fn verify_or_bind(&self, file: &File) -> io::Result<()> {
        let mut identity = self.identity()?;
        if let Some(bound) = *identity {
            bound.verify(file)
        } else {
            *identity = Some(PrivateFileIdentity::capture(file)?);
            Ok(())
        }
    }

    fn identity(&self) -> io::Result<MutexGuard<'_, Option<PrivateFileIdentity>>> {
        self.identity.lock().map_err(|error| {
            io::Error::other(format!(
                "private line-log identity lock is poisoned: {error}"
            ))
        })
    }
}

fn acquire_private_fs_io() -> io::Result<crate::resource::DescriptorPermit> {
    crate::resource::acquire_private_fs().map_err(io::Error::other)
}

fn truncate_incomplete_tail(file: &mut File) -> io::Result<()> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if bytes.last().is_none_or(|last| *last == b'\n') {
        return Ok(());
    }
    let complete_len = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |position| position + 1);
    file.set_len(u64::try_from(complete_len).map_err(io::Error::other)?)
}

#[cfg(test)]
#[path = "private_line_log_tests.rs"]
mod tests;
