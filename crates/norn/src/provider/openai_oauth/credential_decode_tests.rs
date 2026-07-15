use std::error::Error;

use base64::Engine as _;

use super::*;

type TestResult = Result<(), Box<dyn Error>>;

fn id_token(account: Option<&str>) -> String {
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
    let claims = account.map_or_else(
        || serde_json::json!({}),
        |account| {
            serde_json::json!({
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": account
                }
            })
        },
    );
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("{header}.{claims}.")
}

fn document(
    id_token: &str,
    account: Option<&str>,
    access_token: &str,
    refresh_token: &str,
) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&serde_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": id_token,
            "access_token": access_token,
            "refresh_token": refresh_token,
            "account_id": account,
        }
    }))
}

#[test]
fn invalid_json_and_utf8_remain_invalid_json() {
    for raw in [b"{malformed".as_slice(), b"{\"tokens\":\xff}".as_slice()] {
        assert!(matches!(
            decode_auth_dot_json(raw),
            Err(MalformedCredentialReason::InvalidJson)
        ));
    }
}

#[test]
fn account_failures_are_classified_without_serde_error_text() -> TestResult {
    let without_claim = id_token(None);
    let account_a = id_token(Some("account-a"));
    for (raw, reason) in [
        (
            document(&without_claim, None, "access", "refresh")?,
            MalformedCredentialReason::MissingAccountId,
        ),
        (
            document(&account_a, Some("account\nunsafe"), "access", "refresh")?,
            MalformedCredentialReason::InvalidAccountId,
        ),
        (
            document(&account_a, Some("account-b"), "access", "refresh")?,
            MalformedCredentialReason::ConflictingAccountIds,
        ),
    ] {
        assert_eq!(decode_auth_dot_json(&raw), Err(reason));
    }
    Ok(())
}

#[test]
fn malformed_id_token_claims_have_a_distinct_reason() -> TestResult {
    for malformed in ["not-a-jwt", "e30.%%%."] {
        let raw = document(malformed, Some("account-a"), "access", "refresh")?;
        assert_eq!(
            decode_auth_dot_json(&raw),
            Err(MalformedCredentialReason::MalformedIdTokenClaims)
        );
    }
    Ok(())
}

#[test]
fn unsafe_token_fields_keep_field_specific_reasons() -> TestResult {
    let id_token = id_token(Some("account-a"));
    for (access, refresh, reason) in [
        (
            " access-secret",
            "refresh",
            MalformedCredentialReason::InvalidAccessToken,
        ),
        (
            "access",
            "refresh\0secret",
            MalformedCredentialReason::InvalidRefreshToken,
        ),
        ("", "", MalformedCredentialReason::MissingUsableToken),
    ] {
        let raw = document(&id_token, Some("account-a"), access, refresh)?;
        assert_eq!(decode_auth_dot_json(&raw), Err(reason));
        let rendered = format!("{reason:?}");
        if !access.is_empty() {
            assert!(!rendered.contains(access));
        }
        if !refresh.is_empty() {
            assert!(!rendered.contains(refresh));
        }
    }
    Ok(())
}

#[test]
fn document_shape_reasons_and_codex_dual_slots_are_preserved() -> TestResult {
    let id_token = id_token(Some("account-a"));
    let unsupported = serde_json::to_vec(&serde_json::json!({
        "auth_mode": "api-key",
        "tokens": {}
    }))?;
    let mixed = serde_json::to_vec(&serde_json::json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": "direct-key",
        "tokens": {
            "id_token": id_token,
            "access_token": "access",
            "refresh_token": "refresh",
            "account_id": "account-a"
        }
    }))?;
    let missing = serde_json::to_vec(&serde_json::json!({"auth_mode": "chatgpt"}))?;

    assert_eq!(
        decode_auth_dot_json(&unsupported),
        Err(MalformedCredentialReason::UnsupportedAuthMode)
    );
    let decoded = decode_auth_dot_json(&mixed)?;
    assert_eq!(decoded.openai_api_key.as_deref(), Some("direct-key"));
    assert_eq!(
        serde_json::to_value(decoded)?.get("OPENAI_API_KEY"),
        Some(&serde_json::json!("direct-key"))
    );
    assert_eq!(
        decode_auth_dot_json(&missing),
        Err(MalformedCredentialReason::MissingTokenBundle)
    );
    Ok(())
}

#[test]
fn stored_access_and_refresh_tokens_are_independently_optional() -> TestResult {
    let id_token = id_token(Some("account-a"));
    for (access, refresh) in [("", "refresh"), ("access", "")] {
        let raw = document(&id_token, Some("account-a"), access, refresh)?;
        let decoded = decode_auth_dot_json(&raw)?;
        let tokens = decoded
            .tokens
            .ok_or_else(|| std::io::Error::other("decoded token bundle is missing"))?;
        assert_eq!(tokens.access_token, access);
        assert_eq!(tokens.refresh_token, refresh);
    }
    Ok(())
}

#[test]
fn strict_decode_still_owns_noncredential_json_shape() -> TestResult {
    let id_token = id_token(Some("account-a"));
    let raw = serde_json::to_vec(&serde_json::json!({
        "auth_mode": "chatgpt",
        "last_refresh": "not-a-timestamp",
        "future_top_level": {"preserve": true},
        "tokens": {
            "id_token": id_token,
            "access_token": "access",
            "refresh_token": "refresh",
            "account_id": "account-a",
            "future_token_field": [1, 2, 3]
        }
    }))?;

    assert_eq!(
        decode_auth_dot_json(&raw),
        Err(MalformedCredentialReason::InvalidJson)
    );
    Ok(())
}
