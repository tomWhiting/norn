use base64::Engine as _;

use super::*;

fn fixture_jwt(claims: &serde_json::Value) -> String {
    let header =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("{header}.{claims}.")
}

#[test]
fn missing_account_identity_is_an_observable_deserialization_error() -> Result<(), std::io::Error> {
    let id_token = fixture_jwt(&serde_json::json!({"scope": "openid"}));
    let result = serde_json::from_value::<AuthDotJson>(serde_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": id_token,
            "access_token": "not-an-access-token",
            "refresh_token": "not-a-refresh-token"
        }
    }));
    let Err(error) = result else {
        return Err(std::io::Error::other(
            "accountless credentials unexpectedly deserialized",
        ));
    };

    assert!(error.to_string().contains("missing an account identifier"));
    Ok(())
}

#[test]
fn unsafe_token_fields_are_rejected_without_disclosure() -> Result<(), std::io::Error> {
    let id_token = IdTokenInfo {
        raw_jwt: "id-token-secret".to_owned(),
        email: None,
        chatgpt_plan_type: None,
        chatgpt_user_id: None,
        chatgpt_account_id: Some("account-fixture".to_owned()),
    };

    for (access_token, refresh_token, account_id, field) in [
        ("", "refresh", "account", CredentialField::AccessToken),
        (
            "access\rsecret",
            "refresh",
            "account",
            CredentialField::AccessToken,
        ),
        ("access", "", "account", CredentialField::RefreshToken),
        (
            "access",
            " refresh",
            "account",
            CredentialField::RefreshToken,
        ),
        (
            "access",
            "refresh\0secret",
            "account",
            CredentialField::RefreshToken,
        ),
        ("access", "refresh", "", CredentialField::AccountId),
        (
            "access",
            "refresh",
            "account\nsecret",
            CredentialField::AccountId,
        ),
    ] {
        let result = ChatGptTokens::validated(
            id_token.clone(),
            access_token.to_owned(),
            refresh_token.to_owned(),
            account_id.to_owned(),
        );
        let Err(error) = result else {
            return Err(std::io::Error::other(
                "unsafe token bundle unexpectedly passed validation",
            ));
        };
        let rendered = error.to_string();

        assert_eq!(error.field(), field);
        assert!(!rendered.contains("secret"));
    }
    Ok(())
}

#[test]
fn unsafe_stored_fields_are_rejected_during_import() -> Result<(), std::io::Error> {
    let id_token = fixture_jwt(&serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "account-fixture"
        }
    }));

    for (access_token, refresh_token, account_id, expected) in [
        (
            "access\rsecret",
            "refresh",
            "account-fixture",
            "access token",
        ),
        (
            "access",
            "refresh\0secret",
            "account-fixture",
            "refresh token",
        ),
        ("access", "refresh", "", "account identifier"),
        ("access", "refresh", "account\nsecret", "account identifier"),
    ] {
        let result = serde_json::from_value::<AuthDotJson>(serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": id_token.clone(),
                "access_token": access_token,
                "refresh_token": refresh_token,
                "account_id": account_id
            }
        }));
        let Err(error) = result else {
            return Err(std::io::Error::other(
                "unsafe stored credential unexpectedly imported",
            ));
        };
        let rendered = error.to_string();

        assert!(rendered.contains(expected));
        assert!(!rendered.contains("secret"));
    }
    Ok(())
}

#[test]
fn stored_partial_tokens_decode_but_both_empty_do_not() -> Result<(), std::io::Error> {
    let id_token = fixture_jwt(&serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "account-fixture"
        }
    }));

    for (access_token, refresh_token) in [("", "refresh"), ("access", "")] {
        let auth = serde_json::from_value::<AuthDotJson>(serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": id_token.clone(),
                "access_token": access_token,
                "refresh_token": refresh_token
            }
        }))
        .map_err(std::io::Error::other)?;
        let tokens = auth
            .tokens
            .ok_or_else(|| std::io::Error::other("partial token bundle is missing"))?;
        assert_eq!(tokens.access_token, access_token);
        assert_eq!(tokens.refresh_token, refresh_token);
    }

    let result = serde_json::from_value::<AuthDotJson>(serde_json::json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": id_token,
            "access_token": "",
            "refresh_token": ""
        }
    }));
    let Err(error) = result else {
        return Err(std::io::Error::other(
            "empty stored token bundle unexpectedly decoded",
        ));
    };
    assert!(error.to_string().contains("missing a usable token"));
    Ok(())
}

#[test]
fn unknown_fields_round_trip_without_debug_disclosure() -> Result<(), Box<dyn std::error::Error>> {
    let id_token = fixture_jwt(&serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "account-fixture"
        }
    }));
    let input = serde_json::json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": "codex-api-key-secret",
        "tokens": {
            "id_token": id_token,
            "access_token": "not-an-access-token",
            "refresh_token": "not-a-refresh-token",
            "future-token-key-secret": {"value": "future-token-value-secret"}
        },
        "future-top-key-secret": ["future-top-value-secret"]
    });

    let auth = serde_json::from_value::<AuthDotJson>(input.clone())?;
    let tokens = auth
        .tokens
        .as_ref()
        .ok_or_else(|| std::io::Error::other("round-tripped OAuth tokens are missing"))?;
    let serialized = serde_json::to_value(&auth)?;

    assert_eq!(
        required_json_value(&serialized, "/OPENAI_API_KEY")?,
        required_json_value(&input, "/OPENAI_API_KEY")?
    );
    assert_eq!(
        required_json_value(&serialized, "/future-top-key-secret")?,
        required_json_value(&input, "/future-top-key-secret")?
    );
    assert_eq!(
        required_json_value(&serialized, "/tokens/future-token-key-secret")?,
        required_json_value(&input, "/tokens/future-token-key-secret")?
    );
    let rendered = format!("{auth:?} {tokens:?}");
    for secret in [
        "future-top-key-secret",
        "future-top-value-secret",
        "future-token-key-secret",
        "future-token-value-secret",
        "codex-api-key-secret",
    ] {
        assert!(!rendered.contains(secret));
    }
    assert!(rendered.contains("additional_field_count"));
    Ok(())
}

#[test]
fn extension_fields_cannot_override_canonical_fields() -> Result<(), Box<dyn std::error::Error>> {
    let raw_id_token = fixture_jwt(&serde_json::json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": "canonical-account"
        }
    }));
    let id_token = IdTokenInfo::from_raw_jwt(raw_id_token.clone())?;
    let mut tokens = ChatGptTokens::validated(
        id_token,
        "canonical-access".to_owned(),
        "canonical-refresh".to_owned(),
        "canonical-account".to_owned(),
    )?;
    for field in ["id_token", "access_token", "refresh_token", "account_id"] {
        tokens
            .additional_fields
            .insert(field.to_owned(), serde_json::json!("extension-override"));
    }
    tokens.additional_fields.insert(
        "future-token-field".to_owned(),
        serde_json::json!("preserved"),
    );
    let mut auth = AuthDotJson::from_tokens(tokens);
    for field in [
        "auth_mode",
        "OPENAI_API_KEY",
        "tokens",
        "last_refresh",
        "agent_identity",
    ] {
        auth.additional_fields
            .insert(field.to_owned(), serde_json::json!("extension-override"));
    }
    auth.additional_fields.insert(
        "future-top-field".to_owned(),
        serde_json::json!("preserved"),
    );

    let serialized = serde_json::to_value(auth)?;

    assert_eq!(
        required_json_value(&serialized, "/auth_mode")?,
        &serde_json::json!("chatgpt")
    );
    assert_eq!(
        required_json_value(&serialized, "/tokens/id_token")?,
        &serde_json::json!(raw_id_token)
    );
    assert_eq!(
        required_json_value(&serialized, "/tokens/access_token")?,
        &serde_json::json!("canonical-access")
    );
    assert_eq!(
        required_json_value(&serialized, "/tokens/future-token-field")?,
        &serde_json::json!("preserved")
    );
    assert_eq!(
        required_json_value(&serialized, "/future-top-field")?,
        &serde_json::json!("preserved")
    );
    Ok(())
}

fn required_json_value<'a>(
    value: &'a serde_json::Value,
    pointer: &str,
) -> Result<&'a serde_json::Value, std::io::Error> {
    value
        .pointer(pointer)
        .ok_or_else(|| std::io::Error::other(format!("serialized credential is missing {pointer}")))
}
