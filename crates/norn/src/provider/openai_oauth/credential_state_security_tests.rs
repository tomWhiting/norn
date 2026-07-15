use std::collections::BTreeMap;
use std::path::Path;

use super::super::auth_root::{NornAuthRoot, NornAuthRootError};
use super::super::storage::AUTH_JSON_FILE;
use super::super::types::{AuthDotJson, ChatGptTokens, IdTokenInfo};
use super::*;
use base64::Engine as _;

fn norn_auth_root(path: &Path) -> Result<NornAuthRoot, NornAuthRootError> {
    NornAuthRoot::try_from(path)
}

fn now() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::seconds(1_800_000_000)
}

fn auth(access_token: String, refresh_token: &str) -> AuthDotJson {
    AuthDotJson {
        auth_mode: Some("chatgpt".to_owned()),
        openai_api_key: None,
        tokens: Some(ChatGptTokens {
            id_token: IdTokenInfo::create_for_testing("account-fixture"),
            access_token,
            refresh_token: refresh_token.to_owned(),
            account_id: Some("account-fixture".to_owned()),
            additional_fields: BTreeMap::default(),
        }),
        last_refresh: None,
        agent_identity: None,
        additional_fields: BTreeMap::default(),
    }
}

fn access_token_with_claims(claims: &serde_json::Value) -> String {
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("{header}.{claims}.")
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn file_inspection_preserves_typed_decode_reason() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let id_token = access_token_with_claims(&serde_json::json!({}));
    let raw = serde_json::to_vec(&serde_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": id_token,
            "access_token": "access",
            "refresh_token": "refresh"
        }
    }))?;
    std::fs::write(root.path().join(AUTH_JSON_FILE), raw)?;

    let state = inspect_file_credential(
        &norn_auth_root(root.path())?,
        AuthCredentialsStoreMode::File,
        now(),
    )?;

    assert_eq!(
        state,
        LocalCredentialState::Malformed {
            reason: MalformedCredentialReason::MissingAccountId,
        }
    );
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn file_inspection_rejects_symlink_credential() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::fs::symlink;

    let container = tempfile::tempdir()?;
    let root = container.path().join("credential-root");
    std::fs::create_dir(&root)?;
    let target = container.path().join("outside-auth.json");
    std::fs::write(
        &target,
        serde_json::to_vec(&auth("outside-token".to_owned(), "refresh"))?,
    )?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755))?;
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644))?;
    symlink(&target, root.join(AUTH_JSON_FILE))?;

    let result = inspect_file_credential(
        &norn_auth_root(&root)?,
        AuthCredentialsStoreMode::File,
        now(),
    );

    assert!(matches!(
        result,
        Err(CredentialInspectionError::Storage(StorageError::Io(_)))
    ));
    assert_eq!(
        std::fs::metadata(&root)?.permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        std::fs::metadata(&target)?.permissions().mode() & 0o777,
        0o644
    );
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn file_inspection_rejects_symlink_root_without_mutating_target()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let container = tempfile::tempdir()?;
    let real_root = container.path().join("real-root");
    let linked_root = container.path().join("linked-root");
    let auth_path = real_root.join(AUTH_JSON_FILE);
    std::fs::create_dir(&real_root)?;
    std::fs::write(
        &auth_path,
        serde_json::to_vec(&auth("outside-token".to_owned(), "refresh"))?,
    )?;
    std::fs::set_permissions(&real_root, std::fs::Permissions::from_mode(0o755))?;
    std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o644))?;
    symlink(&real_root, &linked_root)?;

    let result = inspect_file_credential(
        &norn_auth_root(&linked_root)?,
        AuthCredentialsStoreMode::File,
        now(),
    );

    assert!(matches!(
        result,
        Err(CredentialInspectionError::Storage(StorageError::Io(_)))
    ));
    assert_eq!(
        std::fs::metadata(&real_root)?.permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        std::fs::metadata(auth_path)?.permissions().mode() & 0o777,
        0o644
    );
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn file_inspection_rejects_non_regular_credential() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir()?;
    let auth_path = root.path().join(AUTH_JSON_FILE);
    std::fs::create_dir(&auth_path)?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755))?;
    std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o755))?;

    let result = inspect_file_credential(
        &norn_auth_root(root.path())?,
        AuthCredentialsStoreMode::File,
        now(),
    );

    assert!(matches!(
        result,
        Err(CredentialInspectionError::Storage(StorageError::Io(_)))
    ));
    assert_eq!(
        std::fs::metadata(root.path())?.permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        std::fs::metadata(auth_path)?.permissions().mode() & 0o777,
        0o755
    );
    Ok(())
}
