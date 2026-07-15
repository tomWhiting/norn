use std::time::Duration;

use base64::Engine as _;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

fn fixture_jwt(claims: &serde_json::Value) -> String {
    let header =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("{header}.{claims}.")
}

fn current_tokens(account_id: &str) -> ChatGptTokens {
    ChatGptTokens {
        id_token: IdTokenInfo {
            raw_jwt: fixture_jwt(&serde_json::json!({
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": account_id
                }
            })),
            email: None,
            chatgpt_plan_type: None,
            chatgpt_user_id: None,
            chatgpt_account_id: Some(account_id.to_owned()),
        },
        access_token: "not-an-access-token".to_owned(),
        refresh_token: "not-a-refresh-token".to_owned(),
        account_id: Some(account_id.to_owned()),
        additional_fields: std::collections::BTreeMap::new(),
    }
}

fn auth() -> AuthDotJson {
    let mut tokens = current_tokens("account-secret");
    tokens.access_token = "access-token-secret".to_owned();
    tokens.refresh_token = "refresh-token-secret".to_owned();
    AuthDotJson::from_tokens(tokens)
}

fn response(id_token: String, account_id: Option<&str>) -> RefreshResponse {
    RefreshResponse {
        id_token,
        access_token: "not-a-refreshed-access-token".to_owned(),
        refresh_token: Some("not-a-rotated-refresh-token".to_owned()),
        account_id: account_id.map(str::to_owned),
    }
}

fn current_tokens_with_user(account_id: &str, user_id: &str) -> Result<ChatGptTokens, JwtError> {
    let mut tokens = current_tokens(account_id);
    tokens.id_token = IdTokenInfo::from_raw_jwt(fixture_jwt(&serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
            "chatgpt_user_id": user_id
        }
    })))?;
    Ok(tokens)
}

#[tokio::test]
async fn malformed_success_response_is_indeterminate_not_retryable()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("malformed-success-body-secret"))
        .mount(&server)
        .await;
    let client = crate::provider::http_client::build_bounded_client(Duration::from_secs(5))?;

    let result = refresh_auth(&auth(), &server.uri(), &client).await;
    let Err(error) = result else {
        return Err(std::io::Error::other(
            "malformed success response unexpectedly refreshed credentials",
        )
        .into());
    };
    let rendered = error.to_string();

    assert!(matches!(error, RefreshError::Indeterminate(_)));
    assert!(rendered.contains("malformed success response"));
    assert!(!rendered.contains("malformed-success-body-secret"));
    Ok(())
}

#[tokio::test]
async fn refresh_preserves_unknown_fields_without_debug_disclosure()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start().await;
    let refreshed_id_token = fixture_jwt(&serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "account-secret"
        }
    }));
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id_token": refreshed_id_token,
            "access_token": "refreshed-access-token",
            "refresh_token": "refreshed-refresh-token",
            "account_id": "account-secret"
        })))
        .mount(&server)
        .await;
    let client = crate::provider::http_client::build_bounded_client(Duration::from_secs(5))?;
    let mut current = auth();
    current.additional_fields.insert(
        "future-top-key-secret".to_owned(),
        serde_json::json!({"value": "future-top-value-secret"}),
    );
    let current_token_fields = {
        let current_tokens = current
            .tokens
            .as_mut()
            .ok_or_else(|| std::io::Error::other("refresh fixture has no tokens"))?;
        current_tokens.additional_fields.insert(
            "future-token-key-secret".to_owned(),
            serde_json::json!(["future-token-value-secret"]),
        );
        current_tokens.additional_fields.clone()
    };

    let updated = refresh_auth(&current, &server.uri(), &client).await?;
    let updated_tokens = updated
        .tokens
        .as_ref()
        .ok_or_else(|| std::io::Error::other("refreshed credential has no tokens"))?;

    assert_eq!(updated_tokens.access_token, "refreshed-access-token");
    assert_eq!(updated_tokens.refresh_token, "refreshed-refresh-token");
    assert_eq!(updated.additional_fields, current.additional_fields);
    assert_eq!(updated_tokens.additional_fields, current_token_fields);
    let rendered = format!("{updated:?} {updated_tokens:?}");
    for secret in [
        "future-top-key-secret",
        "future-top-value-secret",
        "future-token-key-secret",
        "future-token-value-secret",
    ] {
        assert!(!rendered.contains(secret));
    }
    Ok(())
}

#[test]
fn missing_refresh_account_is_indeterminate() -> Result<(), std::io::Error> {
    let current = current_tokens("account-fixture");
    let id_token = fixture_jwt(&serde_json::json!({"scope": "openid"}));
    let result = refreshed_tokens(response(id_token, None), &current);
    let Err(error) = result else {
        return Err(std::io::Error::other(
            "accountless refresh response unexpectedly succeeded",
        ));
    };

    assert!(matches!(error, RefreshError::Indeterminate(_)));
    assert!(error.to_string().contains("no account identity metadata"));
    Ok(())
}

#[test]
fn refreshed_user_cannot_replace_prior_identity() -> Result<(), Box<dyn std::error::Error>> {
    let current = current_tokens_with_user("workspace-fixture", "prior-user-secret")?;
    let id_token = fixture_jwt(&serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "workspace-fixture",
            "chatgpt_user_id": "replacement-user-secret"
        }
    }));

    let result = refreshed_tokens(response(id_token, None), &current);
    let Err(error) = result else {
        return Err(std::io::Error::other("refresh changed the pinned user identity").into());
    };
    let rendered = error.to_string();
    assert!(matches!(error, RefreshError::Indeterminate(_)));
    assert!(rendered.contains("conflicting user identity metadata"));
    assert!(!rendered.contains("prior-user-secret"));
    assert!(!rendered.contains("replacement-user-secret"));
    Ok(())
}

#[test]
fn unsafe_success_fields_are_indeterminate_without_disclosure() -> Result<(), std::io::Error> {
    let current = current_tokens("account-fixture");
    let id_token = fixture_jwt(&serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "account-fixture"
        }
    }));

    for (access_token, refresh_token, account_id, expected) in [
        ("", Some("rotated"), None, "unusable access token"),
        (
            "access\rsecret",
            Some("rotated"),
            None,
            "unusable access token",
        ),
        ("access", Some(""), None, "unusable refresh token"),
        (
            "access",
            Some("rotated\0secret"),
            None,
            "unusable refresh token",
        ),
        (
            "access",
            Some("rotated"),
            Some("account\nsecret"),
            "invalid account identity metadata",
        ),
    ] {
        let result = refreshed_tokens(
            RefreshResponse {
                id_token: id_token.clone(),
                access_token: access_token.to_owned(),
                refresh_token: refresh_token.map(str::to_owned),
                account_id: account_id.map(str::to_owned),
            },
            &current,
        );
        let Err(error) = result else {
            return Err(std::io::Error::other(
                "unsafe refresh response unexpectedly succeeded",
            ));
        };
        let rendered = error.to_string();

        assert!(matches!(error, RefreshError::Indeterminate(_)));
        assert!(rendered.contains(expected));
        assert!(!rendered.contains("secret"));
    }
    Ok(())
}

#[test]
fn invalid_current_refresh_preconditions_are_permanent() {
    let mut missing_account = current_tokens("account-fixture");
    missing_account.account_id = None;
    missing_account.id_token.chatgpt_account_id = None;
    let mut invalid_refresh = current_tokens("account-fixture");
    invalid_refresh.refresh_token = " refresh".to_owned();

    for tokens in [&missing_account, &invalid_refresh] {
        assert!(matches!(
            validate_refresh_preconditions(tokens),
            Err(RefreshError::Permanent(_))
        ));
    }
}
