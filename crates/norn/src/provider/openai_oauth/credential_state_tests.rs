use std::collections::BTreeMap;
use std::path::Path;

use super::super::auth_root::{NornAuthRoot, NornAuthRootError};
use super::super::storage::{AUTH_JSON_FILE, load_auth_dot_json};
use super::super::types::{AuthDotJson, ChatGptTokens, IdTokenInfo};
use super::*;
use base64::Engine as _;

fn norn_auth_root(path: &Path) -> Result<NornAuthRoot, NornAuthRootError> {
    NornAuthRoot::try_from(path)
}

fn now() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH + chrono::TimeDelta::seconds(1_800_000_000)
}

fn access_token(exp: Option<i64>) -> String {
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
    let claims = exp.map_or_else(
        || serde_json::json!({}),
        |value| serde_json::json!({"exp": value}),
    );
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("{header}.{claims}.")
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

#[test]
fn known_future_expiry_is_locally_valid() {
    let state = evaluate_chatgpt_credential(&auth(access_token(Some(1_800_000_001)), "r"), now());
    assert!(matches!(state, LocalCredentialState::LocallyValid { .. }));
}

#[test]
fn expiry_at_now_requires_refresh() {
    let state = evaluate_chatgpt_credential(&auth(access_token(Some(1_800_000_000)), "r"), now());
    assert!(matches!(
        state,
        LocalCredentialState::RefreshCandidate {
            reason: RefreshCandidateReason::AccessExpired,
            ..
        }
    ));
}

#[test]
fn expired_access_without_refresh_is_not_dispatchable() {
    let state = evaluate_chatgpt_credential(&auth(access_token(Some(1_799_999_999)), ""), now());
    assert!(matches!(state, LocalCredentialState::AccessExpired { .. }));
}

#[test]
fn missing_access_with_refresh_is_a_candidate() {
    let state = evaluate_chatgpt_credential(&auth(String::new(), "refresh"), now());
    assert!(matches!(
        state,
        LocalCredentialState::RefreshCandidate {
            reason: RefreshCandidateReason::AccessMissing,
            expired_at: None,
        }
    ));
}

#[test]
fn missing_access_and_refresh_is_malformed() {
    let state = evaluate_chatgpt_credential(&auth(String::new(), ""), now());
    assert_eq!(
        state,
        LocalCredentialState::Malformed {
            reason: MalformedCredentialReason::MissingUsableToken,
        }
    );
}

#[test]
fn opaque_and_missing_expiry_are_unknown() {
    let opaque = evaluate_chatgpt_credential(&auth("opaque-token".to_owned(), "refresh"), now());
    let missing_expiry = evaluate_chatgpt_credential(&auth(access_token(None), "refresh"), now());
    assert_eq!(
        opaque,
        LocalCredentialState::Unknown {
            reason: UnknownExpiryReason::OpaqueAccessToken,
        }
    );
    assert_eq!(
        missing_expiry,
        LocalCredentialState::Unknown {
            reason: UnknownExpiryReason::MissingExpiration,
        }
    );
}

#[test]
fn malformed_jwt_shaped_access_tokens_are_malformed() {
    let invalid_exp = access_token_with_claims(&serde_json::json!({"exp": "invalid"}));
    for access_token in ["header.claims", "e30.%%%.c2ln", invalid_exp.as_str()] {
        assert_eq!(
            evaluate_chatgpt_credential(&auth(access_token.to_owned(), "refresh"), now()),
            LocalCredentialState::Malformed {
                reason: MalformedCredentialReason::MalformedAccessTokenClaims,
            }
        );
    }
}

#[test]
fn unsafe_access_and_refresh_values_are_malformed() {
    for (access_token, refresh_token, reason) in [
        (
            "opaque\raccess",
            "refresh",
            MalformedCredentialReason::InvalidAccessToken,
        ),
        (
            " opaque",
            "refresh",
            MalformedCredentialReason::InvalidAccessToken,
        ),
        (
            "opaque",
            "refresh\0secret",
            MalformedCredentialReason::InvalidRefreshToken,
        ),
        (
            "opaque",
            "refresh ",
            MalformedCredentialReason::InvalidRefreshToken,
        ),
    ] {
        assert_eq!(
            evaluate_chatgpt_credential(&auth(access_token.to_owned(), refresh_token), now()),
            LocalCredentialState::Malformed { reason }
        );
    }
}

#[test]
fn missing_invalid_and_conflicting_accounts_are_distinct() -> Result<(), std::io::Error> {
    let mut missing = auth("opaque".to_owned(), "refresh");
    let Some(missing_tokens) = missing.tokens.as_mut() else {
        return Err(std::io::Error::other("fixture omitted token bundle"));
    };
    missing_tokens.account_id = None;
    missing_tokens.id_token.chatgpt_account_id = None;
    assert_eq!(
        evaluate_chatgpt_credential(&missing, now()),
        LocalCredentialState::Malformed {
            reason: MalformedCredentialReason::MissingAccountId,
        }
    );

    let mut invalid = auth("opaque".to_owned(), "refresh");
    let Some(invalid_tokens) = invalid.tokens.as_mut() else {
        return Err(std::io::Error::other("fixture omitted token bundle"));
    };
    invalid_tokens.account_id = Some("account\nsecret".to_owned());
    assert_eq!(
        evaluate_chatgpt_credential(&invalid, now()),
        LocalCredentialState::Malformed {
            reason: MalformedCredentialReason::InvalidAccountId,
        }
    );

    let mut conflicting = auth("opaque".to_owned(), "refresh");
    let Some(conflicting_tokens) = conflicting.tokens.as_mut() else {
        return Err(std::io::Error::other("fixture omitted token bundle"));
    };
    conflicting_tokens.account_id = Some("different-account".to_owned());
    assert_eq!(
        evaluate_chatgpt_credential(&conflicting, now()),
        LocalCredentialState::Malformed {
            reason: MalformedCredentialReason::ConflictingAccountIds,
        }
    );
    Ok(())
}

#[test]
fn last_refresh_never_changes_unknown_expiry_policy() {
    let mut document = auth("opaque-token".to_owned(), "refresh");
    let baseline = evaluate_chatgpt_credential(&document, now());
    for timestamp in [1_700_000_000, 1_799_000_000, 1_800_000_000] {
        document.last_refresh = DateTime::from_timestamp(timestamp, 0);
        assert_eq!(evaluate_chatgpt_credential(&document, now()), baseline);
    }
}

#[test]
fn missing_bundle_is_malformed_but_codex_dual_slots_are_valid() {
    let mut missing = auth("opaque-token".to_owned(), "refresh");
    missing.tokens = None;
    assert!(matches!(
        evaluate_chatgpt_credential(&missing, now()),
        LocalCredentialState::Malformed {
            reason: MalformedCredentialReason::MissingTokenBundle,
        }
    ));

    let mut mixed = auth("opaque-token".to_owned(), "refresh");
    mixed.openai_api_key = Some("direct-key".to_owned());
    assert_eq!(
        evaluate_chatgpt_credential(&mixed, now()),
        LocalCredentialState::Unknown {
            reason: UnknownExpiryReason::OpaqueAccessToken,
        }
    );
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn file_inspection_preserves_root_and_credential_modes() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir()?;
    let auth_path = root.path().join(AUTH_JSON_FILE);
    std::fs::write(
        &auth_path,
        serde_json::to_vec(&auth("opaque-token".to_owned(), "refresh"))?,
    )?;
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755))?;
    std::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o644))?;

    let state = inspect_file_credential(
        &norn_auth_root(root.path())?,
        AuthCredentialsStoreMode::File,
        now(),
    )?;

    assert!(matches!(
        state,
        LocalCredentialState::Unknown {
            reason: UnknownExpiryReason::OpaqueAccessToken,
        }
    ));
    assert_eq!(
        std::fs::metadata(root.path())?.permissions().mode() & 0o777,
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
fn codex_dual_slot_partial_credentials_round_trip_and_classify()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let auth_path = root.path().join(AUTH_JSON_FILE);
    let id_token = access_token_with_claims(&serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "workspace-fixture",
            "chatgpt_user_id": "user-fixture"
        }
    }));
    let refresh_only = serde_json::json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": "codex-derived-api-key",
        "tokens": {
            "id_token": id_token,
            "access_token": "",
            "refresh_token": "refresh-fixture",
            "account_id": "workspace-fixture",
            "future_token_field": {"preserved": true}
        },
        "future_top_field": ["preserved"]
    });
    std::fs::write(&auth_path, serde_json::to_vec(&refresh_only)?)?;

    let typed_root = norn_auth_root(root.path())?;
    let loaded = load_auth_dot_json(&typed_root, AuthCredentialsStoreMode::File)?
        .ok_or_else(|| std::io::Error::other("file-backed credential is missing"))?;
    let serialized = serde_json::to_value(&loaded)?;
    assert_eq!(
        serialized.get("OPENAI_API_KEY"),
        refresh_only.get("OPENAI_API_KEY")
    );
    assert_eq!(
        serialized.get("future_top_field"),
        refresh_only.get("future_top_field")
    );
    assert_eq!(
        loaded
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.id_token.chatgpt_user_id.as_deref()),
        Some("user-fixture")
    );
    assert!(matches!(
        inspect_file_credential(&typed_root, AuthCredentialsStoreMode::File, now())?,
        LocalCredentialState::RefreshCandidate {
            reason: RefreshCandidateReason::AccessMissing,
            expired_at: None,
        }
    ));

    let access_only = serde_json::json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": "codex-derived-api-key",
        "tokens": {
            "id_token": id_token,
            "access_token": access_token(Some(1_799_999_999)),
            "refresh_token": "",
            "account_id": "workspace-fixture"
        }
    });
    std::fs::write(auth_path, serde_json::to_vec(&access_only)?)?;
    assert!(matches!(
        inspect_file_credential(&typed_root, AuthCredentialsStoreMode::File, now())?,
        LocalCredentialState::AccessExpired { .. }
    ));
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn file_inspection_does_not_create_a_missing_root() -> Result<(), Box<dyn std::error::Error>> {
    let container = tempfile::tempdir()?;
    let missing_root = container.path().join("missing-auth-root");

    let state = inspect_file_credential(
        &norn_auth_root(&missing_root)?,
        AuthCredentialsStoreMode::File,
        now(),
    )?;

    assert_eq!(state, LocalCredentialState::Missing);
    assert!(!missing_root.exists());
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[test]
fn file_inspection_classifies_invalid_utf8_as_malformed_json()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    std::fs::write(root.path().join(AUTH_JSON_FILE), b"{\"tokens\":\xff}")?;

    let state = inspect_file_credential(
        &norn_auth_root(root.path())?,
        AuthCredentialsStoreMode::File,
        now(),
    )?;

    assert_eq!(
        state,
        LocalCredentialState::Malformed {
            reason: MalformedCredentialReason::InvalidJson,
        }
    );
    Ok(())
}

fn access_token_with_claims(claims: &serde_json::Value) -> String {
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("{header}.{claims}.")
}
