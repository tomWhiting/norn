//! Codex CLI-compatible auth storage.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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

/// Atomically persists `auth.json`.
///
/// The file is shared with the Codex CLI, so it must never be observed
/// half-written: the document is written to a unique temp file in the
/// same directory (created with mode `0o600` on Unix), fsynced, and
/// renamed over the destination. Rename within a directory is atomic,
/// so concurrent readers see either the old or the new document.
pub(crate) fn save_auth_dot_json(
    codex_home: &Path,
    auth: &AuthDotJson,
) -> Result<(), StorageError> {
    let _descriptor_permit =
        crate::resource::acquire_private_fs().map_err(std::io::Error::other)?;
    std::fs::create_dir_all(codex_home)?;
    let path = auth_json_path(codex_home);
    let json = serde_json::to_vec_pretty(auth)?;
    let tmp_path = temp_json_path(codex_home);

    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }

    let result = (|| -> Result<(), StorageError> {
        let mut file = options.open(&tmp_path)?;
        file.write_all(&json)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp_path, &path)?;
        Ok(())
    })();

    if result.is_err()
        && let Err(cleanup_err) = std::fs::remove_file(&tmp_path)
        && cleanup_err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            tmp_path = %tmp_path.display(),
            error = %cleanup_err,
            "failed to clean up temporary auth file after write failure"
        );
    }
    result
}

pub(crate) fn delete_auth_dot_json(codex_home: &Path) -> Result<(), std::io::Error> {
    let _descriptor_permit =
        crate::resource::acquire_private_fs().map_err(std::io::Error::other)?;
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

/// Builds a temp-file path unique across processes (pid) and within
/// this process (monotonic counter), in the same directory as the
/// destination so the final rename stays on one filesystem.
fn temp_json_path(codex_home: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    codex_home.join(format!("{AUTH_JSON_FILE}.{}.{seq}.tmp", std::process::id()))
}

fn load_file(codex_home: &Path) -> Result<Option<AuthDotJson>, std::io::Error> {
    let _descriptor_permit =
        crate::resource::acquire_private_fs().map_err(std::io::Error::other)?;
    let path = auth_json_path(codex_home);
    match std::fs::read_to_string(path) {
        Ok(raw) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::super::types::{AuthDotJson, ChatGptTokens, IdTokenInfo};
    use super::*;

    fn auth_doc(access_token: &str) -> AuthDotJson {
        let mut doc = AuthDotJson::from_tokens(ChatGptTokens {
            id_token: IdTokenInfo::from_raw_jwt("id-token".to_string()),
            access_token: access_token.to_string(),
            refresh_token: "refresh-token".to_string(),
            account_id: Some("account".to_string()),
        });
        // Deterministic timestamp so documents built from the same token
        // compare equal across calls.
        doc.last_refresh =
            Some(chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap_or_default());
        doc
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let doc = auth_doc("token-a");
        save_auth_dot_json(dir.path(), &doc).expect("save");
        let loaded = load_auth_dot_json(dir.path(), AuthCredentialsStoreMode::File)
            .expect("load")
            .expect("present");
        assert_eq!(loaded, doc);
    }

    /// Regression test for REVIEW.md medium `openai_oauth/storage.rs:44`:
    /// the save must replace `auth.json` via temp-file + rename, never by
    /// truncating in place — the file is shared with the Codex CLI and a
    /// concurrent reader must never observe a half-written document. A
    /// rename swaps the inode; truncate-in-place keeps it.
    #[cfg(unix)]
    #[test]
    fn save_replaces_file_via_rename_not_truncate_in_place() {
        use std::os::unix::fs::MetadataExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        save_auth_dot_json(dir.path(), &auth_doc("token-a")).expect("first save");
        let first_inode = std::fs::metadata(auth_json_path(dir.path()))
            .expect("metadata")
            .ino();

        save_auth_dot_json(dir.path(), &auth_doc("token-b")).expect("second save");
        let second_inode = std::fs::metadata(auth_json_path(dir.path()))
            .expect("metadata")
            .ino();

        assert_ne!(
            first_inode, second_inode,
            "auth.json must be replaced by rename (new inode), not truncated in place"
        );

        let loaded = load_auth_dot_json(dir.path(), AuthCredentialsStoreMode::File)
            .expect("load")
            .expect("present");
        assert_eq!(loaded, auth_doc("token-b"));
    }

    #[cfg(unix)]
    #[test]
    fn save_preserves_owner_only_mode() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        save_auth_dot_json(dir.path(), &auth_doc("token-a")).expect("save");
        // Overwrite to confirm the replacement file also carries 0o600.
        save_auth_dot_json(dir.path(), &auth_doc("token-b")).expect("overwrite");

        let mode = std::fs::metadata(auth_json_path(dir.path()))
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "auth.json must remain owner-read/write only"
        );
    }

    #[test]
    fn save_leaves_no_temp_files_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        save_auth_dot_json(dir.path(), &auth_doc("token-a")).expect("save");
        save_auth_dot_json(dir.path(), &auth_doc("token-b")).expect("overwrite");

        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .expect("read dir")
            .filter_map(|entry| {
                entry
                    .ok()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
            })
            .filter(|name| name != AUTH_JSON_FILE)
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp files may remain after save: {leftovers:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_save_cleans_up_temp_file_and_reports_error() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().expect("tempdir");
        save_auth_dot_json(dir.path(), &auth_doc("token-a")).expect("seed save");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500))
            .expect("make dir read-only");

        let result = save_auth_dot_json(dir.path(), &auth_doc("token-b"));

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("restore permissions");
        assert!(result.is_err(), "save into read-only dir must fail");

        // Original file untouched, no temp litter.
        let loaded = load_auth_dot_json(dir.path(), AuthCredentialsStoreMode::File)
            .expect("load")
            .expect("present");
        assert_eq!(loaded, auth_doc("token-a"));
    }
}
