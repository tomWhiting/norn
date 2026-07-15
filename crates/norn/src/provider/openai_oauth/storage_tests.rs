use std::error::Error;
use std::path::Path;

use super::super::auth_root::{NornAuthRoot, NornAuthRootError};
use super::super::credential_decode::MalformedCredentialReason;
use super::super::types::{AuthDotJson, ChatGptTokens, IdTokenInfo};
use super::*;

type TestResult = Result<(), Box<dyn Error>>;

fn norn_auth_root(path: &Path) -> Result<NornAuthRoot, NornAuthRootError> {
    NornAuthRoot::try_from(path)
}

fn auth_doc(access_token: &str) -> AuthDotJson {
    let mut doc = AuthDotJson::from_tokens(ChatGptTokens {
        id_token: IdTokenInfo::create_for_testing("account"),
        access_token: access_token.to_owned(),
        refresh_token: "refresh-token".to_owned(),
        account_id: Some("account".to_owned()),
        additional_fields: std::collections::BTreeMap::new(),
    });
    doc.last_refresh = chrono::DateTime::from_timestamp(1_700_000_000, 0);
    doc
}

fn require_auth(value: Option<AuthDotJson>) -> Result<AuthDotJson, std::io::Error> {
    value.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "auth.json missing"))
}

#[test]
fn save_then_load_round_trips() -> TestResult {
    let dir = tempfile::tempdir()?;
    let doc = auth_doc("token-a");
    save_auth_dot_json(dir.path(), &doc)?;
    let loaded = require_auth(load_auth_dot_json(
        &norn_auth_root(dir.path())?,
        AuthCredentialsStoreMode::File,
    )?)?;
    assert_eq!(loaded, doc);
    Ok(())
}

#[test]
fn load_preserves_typed_credential_decode_failures() -> TestResult {
    let dir = tempfile::tempdir()?;
    let raw = serde_json::to_vec(&serde_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": "malformed-id-token-secret",
            "access_token": "access-secret",
            "refresh_token": "refresh-secret",
            "account_id": "account"
        }
    }))?;
    std::fs::write(auth_json_path(dir.path()), raw)?;

    let result = load_auth_dot_json(&norn_auth_root(dir.path())?, AuthCredentialsStoreMode::File);

    let error = match result {
        Err(
            error @ StorageError::MalformedCredential(
                MalformedCredentialReason::MalformedIdTokenClaims,
            ),
        ) => error,
        other => {
            return Err(std::io::Error::other(format!(
                "typed malformed credential error missing: {other:?}"
            ))
            .into());
        }
    };
    let rendered = error.to_string();
    assert!(!rendered.contains("malformed-id-token-secret"));
    assert!(!rendered.contains("access-secret"));
    assert!(!rendered.contains("refresh-secret"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn save_replaces_file_via_rename_not_truncate_in_place() -> TestResult {
    use std::os::unix::fs::MetadataExt as _;

    let dir = tempfile::tempdir()?;
    save_auth_dot_json(dir.path(), &auth_doc("token-a"))?;
    let first_inode = std::fs::metadata(auth_json_path(dir.path()))?.ino();

    save_auth_dot_json(dir.path(), &auth_doc("token-b"))?;
    let second_inode = std::fs::metadata(auth_json_path(dir.path()))?.ino();

    assert_ne!(
        first_inode, second_inode,
        "auth.json must be replaced by rename, not truncated in place"
    );
    let loaded = require_auth(load_auth_dot_json(
        &norn_auth_root(dir.path())?,
        AuthCredentialsStoreMode::File,
    )?)?;
    assert_eq!(loaded, auth_doc("token-b"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn save_preserves_owner_only_mode() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir()?;
    save_auth_dot_json(dir.path(), &auth_doc("token-a"))?;
    save_auth_dot_json(dir.path(), &auth_doc("token-b"))?;

    let mode = std::fs::metadata(auth_json_path(dir.path()))?
        .permissions()
        .mode();
    assert_eq!(
        mode & 0o777,
        0o600,
        "auth.json must remain owner-read/write only"
    );
    Ok(())
}

#[test]
fn save_leaves_no_temp_files_behind() -> TestResult {
    let dir = tempfile::tempdir()?;
    save_auth_dot_json(dir.path(), &auth_doc("token-a"))?;
    save_auth_dot_json(dir.path(), &auth_doc("token-b"))?;

    let entries = std::fs::read_dir(dir.path())?
        .map(|entry| entry.map(|value| value.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, _>>()?;
    let leftovers = entries
        .into_iter()
        .filter(|name| name != AUTH_JSON_FILE)
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "no temp files may remain after save: {leftovers:?}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn failed_save_cleans_up_temp_file_and_reports_error() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir()?;
    save_auth_dot_json(dir.path(), &auth_doc("token-a"))?;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500))?;

    let result = save_auth_dot_json(dir.path(), &auth_doc("token-b"));

    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
    assert!(result.is_err(), "save into read-only dir must fail");
    let loaded = require_auth(load_auth_dot_json(
        &norn_auth_root(dir.path())?,
        AuthCredentialsStoreMode::File,
    )?)?;
    assert_eq!(loaded, auth_doc("token-a"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn load_rejects_symlink_credential() -> TestResult {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir()?;
    let target = dir.path().join("outside-auth.json");
    std::fs::write(&target, serde_json::to_vec(&auth_doc("outside-token"))?)?;
    symlink(&target, auth_json_path(dir.path()))?;

    let result = load_auth_dot_json(&norn_auth_root(dir.path())?, AuthCredentialsStoreMode::File);
    assert!(
        matches!(result, Err(StorageError::Io(_))),
        "a symlink credential must not be followed: {result:?}"
    );
    Ok(())
}

#[test]
fn load_rejects_non_regular_credential() -> TestResult {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir(auth_json_path(dir.path()))?;

    let result = load_auth_dot_json(&norn_auth_root(dir.path())?, AuthCredentialsStoreMode::File);
    assert!(
        matches!(result, Err(StorageError::Io(_))),
        "a directory cannot be loaded as a credential: {result:?}"
    );
    Ok(())
}
