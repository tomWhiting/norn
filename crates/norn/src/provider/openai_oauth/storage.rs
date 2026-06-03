//! Codex CLI-compatible auth storage.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use super::types::AuthDotJson;

/// Auth file name under `$CODEX_HOME`.
pub const AUTH_JSON_FILE: &str = "auth.json";

/// Compatibility enum matching the Codex credential storage selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthCredentialsStoreMode {
    /// Persist credentials in `$CODEX_HOME/auth.json`.
    File,
}

/// Errors from loading or saving auth storage.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// Filesystem operation failed.
    #[error("auth storage I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization or deserialization failed.
    #[error("auth storage JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

/// Loads `$CODEX_HOME/auth.json`, returning `None` when the file is absent.
///
/// # Errors
///
/// Returns [`std::io::Error`] for read failures or malformed JSON.
pub fn load_auth_dot_json(
    codex_home: &Path,
    mode: AuthCredentialsStoreMode,
) -> Result<Option<AuthDotJson>, std::io::Error> {
    match mode {
        AuthCredentialsStoreMode::File => load_file(codex_home),
    }
}

pub(crate) fn save_auth_dot_json(
    codex_home: &Path,
    auth: &AuthDotJson,
) -> Result<(), StorageError> {
    std::fs::create_dir_all(codex_home)?;
    let path = auth_json_path(codex_home);
    let json = serde_json::to_vec_pretty(auth)?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&json)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

pub(crate) fn delete_auth_dot_json(codex_home: &Path) -> Result<(), std::io::Error> {
    let path = auth_json_path(codex_home);
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

pub(crate) fn auth_json_path(codex_home: &Path) -> PathBuf {
    codex_home.join(AUTH_JSON_FILE)
}

fn load_file(codex_home: &Path) -> Result<Option<AuthDotJson>, std::io::Error> {
    let path = auth_json_path(codex_home);
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}
