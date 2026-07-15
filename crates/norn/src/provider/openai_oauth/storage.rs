//! Norn-owned storage for Codex-compatible OAuth credentials.

use std::fs::File;
use std::io::Read as _;
#[cfg(test)]
use std::io::Write as _;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

use super::auth_root::NornAuthRoot;
use super::credential_decode::{MalformedCredentialReason, decode_auth_dot_json};
use super::types::AuthDotJson;
use crate::util::PrivateRoot;

/// Auth file name under the resolved Norn auth root.
pub const AUTH_JSON_FILE: &str = "auth.json";

/// Norn credential-storage selector.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AuthCredentialsStoreMode {
    /// Persist credentials in `$NORN_HOME/auth/auth.json` by default.
    File,
}

/// Errors from loading or saving auth storage.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Filesystem operation failed.
    #[error("auth storage I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization failed.
    #[error("auth storage JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    /// Present credential bytes were structurally or semantically unusable.
    #[error("auth storage credential is malformed")]
    MalformedCredential(MalformedCredentialReason),
}

/// Result of removing the file-backed Norn credential.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteAuthOutcome {
    /// No credential file existed.
    Absent,
    /// The credential file was removed and the containing directory synced.
    Removed,
}

/// Loads `auth.json` from a validated Norn-owned auth root, returning `None`
/// when absent.
///
/// # Errors
///
/// The [`NornAuthRoot`] boundary prevents callers from selecting a relative or
/// undeclared foreign credential directory. This normal provider path may
/// harden the credential root and file modes.
/// Returns [`StorageError`] for read failures or malformed credentials.
pub fn load_auth_dot_json(
    auth_root: &NornAuthRoot,
    mode: AuthCredentialsStoreMode,
) -> Result<Option<AuthDotJson>, StorageError> {
    match mode {
        AuthCredentialsStoreMode::File => load_file(auth_root.as_path()),
    }
}

/// Observes `auth.json` in the Norn auth root without mutating storage.
///
/// Reserved for local status and diagnostics. Provider and transaction paths
/// must use the normal hardening reader instead.
pub(super) fn load_auth_dot_json_observational(
    auth_root: &NornAuthRoot,
    mode: AuthCredentialsStoreMode,
) -> Result<Option<AuthDotJson>, StorageError> {
    match mode {
        AuthCredentialsStoreMode::File => load_file_observational(auth_root.as_path()),
    }
}

/// Atomically persists `auth.json`.
///
/// The document is written to a unique temp file in the same directory
/// (created with mode `0o600` on Unix), fsynced, and renamed over the
/// destination. Rename within a directory is atomic, so concurrent Norn
/// readers see either the old or the new document.
#[cfg(test)]
pub(crate) fn save_auth_dot_json(auth_root: &Path, auth: &AuthDotJson) -> Result<(), StorageError> {
    let descriptor_permit = crate::resource::acquire_private_fs()
        .map_err(std::io::Error::other)
        .map_err(StorageError::Io)?;
    let root = PrivateRoot::create(auth_root)?;
    let json = serde_json::to_vec_pretty(auth)?;
    let tmp_path = temp_json_path();

    let result = (|| -> Result<(), StorageError> {
        let mut file = root.create_new(&tmp_path)?;
        file.write_all(&json)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        root.rename(&tmp_path, Path::new(AUTH_JSON_FILE))?;
        root.sync_dir(Path::new(""))?;
        Ok(())
    })();

    if result.is_err()
        && let Err(cleanup_err) = root.remove_file(&tmp_path)
        && cleanup_err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            tmp_path = %root.display_path(&tmp_path).display(),
            error = %cleanup_err,
            "failed to clean up temporary auth file after write failure"
        );
    }
    drop(descriptor_permit);
    result
}

#[cfg(test)]
pub(crate) fn auth_json_path(auth_root: &Path) -> PathBuf {
    auth_root.join(AUTH_JSON_FILE)
}

/// Builds a temp-file path unique across processes (pid) and within
/// this process (monotonic counter), in the same directory as the
/// destination so the final rename stays on one filesystem.
#[cfg(test)]
fn temp_json_path() -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!("{AUTH_JSON_FILE}.{}.{seq}.tmp", std::process::id()))
}

fn load_file(auth_root: &Path) -> Result<Option<AuthDotJson>, StorageError> {
    let descriptor_permit = crate::resource::acquire_private_fs()
        .map_err(std::io::Error::other)
        .map_err(StorageError::Io)?;
    let result = (|| {
        let root = match PrivateRoot::open(auth_root) {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error)),
        };
        let file = match root.open_read(Path::new(AUTH_JSON_FILE)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(error)),
        };
        read_auth_file(file).map(Some)
    })();
    drop(descriptor_permit);
    result
}

fn load_file_observational(auth_root: &Path) -> Result<Option<AuthDotJson>, StorageError> {
    let descriptor_permit = crate::resource::acquire_private_fs()
        .map_err(std::io::Error::other)
        .map_err(StorageError::Io)?;
    let result = match PrivateRoot::open_read_observational(auth_root, Path::new(AUTH_JSON_FILE)) {
        Ok(file) => read_auth_file(file).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StorageError::Io(error)),
    };
    drop(descriptor_permit);
    result
}

fn read_auth_file(mut file: File) -> Result<AuthDotJson, StorageError> {
    let mut raw = Vec::new();
    file.read_to_end(&mut raw)?;
    decode_auth_dot_json(&raw).map_err(StorageError::MalformedCredential)
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
