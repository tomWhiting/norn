//! Codex CLI-compatible auth data types.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::credential_validation::{
    CredentialField, CredentialValueError, validate_credential_value,
};
use super::jwt::{IdTokenClaims, JwtError, parse_id_token_claims, reconcile_required_account_ids};

/// Top-level `auth.json` shape compatible with the Codex CLI.
#[derive(Clone, Deserialize, PartialEq, Eq)]
pub struct AuthDotJson {
    /// Authentication mode. `ChatGPT` OAuth files use `chatgpt`.
    pub auth_mode: Option<String>,
    /// Compatibility API-key slot that Codex may persist alongside OAuth tokens.
    #[serde(rename = "OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,
    /// OAuth tokens, when logged in via `ChatGPT`.
    pub tokens: Option<ChatGptTokens>,
    /// Last successful token refresh timestamp.
    pub last_refresh: Option<chrono::DateTime<chrono::Utc>>,
    /// Reserved identity field retained for serde compatibility.
    pub agent_identity: Option<serde_json::Value>,
    /// Fields owned by newer or alternate Codex clients.
    #[serde(default, flatten)]
    pub additional_fields: BTreeMap<String, serde_json::Value>,
}

/// Token bundle stored under `tokens` in `auth.json`.
#[derive(Clone, PartialEq, Eq)]
pub struct ChatGptTokens {
    /// Raw id-token JWT string.
    pub id_token: IdTokenInfo,
    /// Raw access-token JWT string.
    pub access_token: String,
    /// Opaque refresh token.
    pub refresh_token: String,
    /// Normalized `ChatGPT` account id. Decoded and newly issued bundles
    /// always contain this; `Option` preserves the external JSON shape.
    pub account_id: Option<String>,
    /// Token fields owned by newer or alternate Codex clients.
    pub additional_fields: BTreeMap<String, serde_json::Value>,
}

impl ChatGptTokens {
    /// Builds a token bundle whose request and refresh fields are usable.
    ///
    /// # Errors
    ///
    /// Returns [`CredentialValueError`] when a field cannot be used safely.
    pub(super) fn validated(
        id_token: IdTokenInfo,
        access_token: String,
        refresh_token: String,
        account_id: String,
    ) -> Result<Self, CredentialValueError> {
        for (field, value) in [
            (CredentialField::AccessToken, access_token.as_str()),
            (CredentialField::RefreshToken, refresh_token.as_str()),
            (CredentialField::AccountId, account_id.as_str()),
        ] {
            validate_credential_value(field, value)?;
        }

        Ok(Self {
            id_token,
            access_token,
            refresh_token,
            account_id: Some(account_id),
            additional_fields: BTreeMap::new(),
        })
    }
}

/// Parsed id-token metadata plus the original JWT for re-serialization.
#[derive(Clone, PartialEq, Eq)]
pub struct IdTokenInfo {
    /// Original id-token JWT string.
    pub raw_jwt: String,
    /// Email claim, when present.
    pub email: Option<String>,
    /// `ChatGPT` plan type claim, when present.
    pub chatgpt_plan_type: Option<String>,
    /// `ChatGPT` user id claim, when present.
    pub chatgpt_user_id: Option<String>,
    /// `ChatGPT` account id claim, when present.
    pub chatgpt_account_id: Option<String>,
}

impl IdTokenInfo {
    /// Creates id-token info from a raw JWT.
    ///
    /// # Errors
    ///
    /// Returns [`JwtError`] when metadata cannot be parsed or a supported
    /// account or user identity source is invalid or conflicts with another.
    pub fn from_raw_jwt(raw_jwt: String) -> Result<Self, JwtError> {
        let claims = parse_id_token_claims(&raw_jwt)?;
        Ok(Self::from_claims(raw_jwt, claims))
    }

    fn from_claims(raw_jwt: String, claims: IdTokenClaims) -> Self {
        Self {
            raw_jwt,
            email: claims.email,
            chatgpt_plan_type: claims.chatgpt_plan_type,
            chatgpt_user_id: claims.chatgpt_user_id,
            chatgpt_account_id: claims.chatgpt_account_id,
        }
    }

    /// Reconciles an account id supplied alongside the id token with its claim.
    pub(super) fn reconcile_account_id(
        &self,
        account_id: Option<String>,
    ) -> Result<String, JwtError> {
        reconcile_required_account_ids(account_id, self.chatgpt_account_id.clone())
    }

    #[cfg(test)]
    pub(crate) fn create_for_testing(account_id: &str) -> Self {
        use base64::Engine as _;

        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"none","typ":"JWT"}"#);
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::json!({
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": account_id
                }
            })
            .to_string(),
        );
        Self {
            raw_jwt: format!("{header}.{claims}."),
            email: None,
            chatgpt_plan_type: None,
            chatgpt_user_id: None,
            chatgpt_account_id: Some(account_id.to_owned()),
        }
    }
}

impl AuthDotJson {
    /// Creates a `ChatGPT` auth document from a token bundle.
    #[must_use]
    pub fn from_tokens(tokens: ChatGptTokens) -> Self {
        Self {
            auth_mode: Some("chatgpt".to_string()),
            openai_api_key: None,
            tokens: Some(tokens),
            last_refresh: Some(chrono::Utc::now()),
            agent_identity: None,
            additional_fields: BTreeMap::new(),
        }
    }
}

#[path = "types_serde.rs"]
mod serde_impls;

/// In-memory credential wrapper used by `AuthManager`.
#[derive(Clone, PartialEq, Eq)]
pub enum CodexAuth {
    /// `ChatGPT` OAuth credentials.
    ChatGpt(Box<AuthDotJson>),
    /// Direct API key credentials, retained for test helper compatibility.
    ApiKey(String),
}

impl std::fmt::Debug for AuthDotJson {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthDotJson")
            .field("auth_mode", &self.auth_mode)
            .field("openai_api_key_present", &self.openai_api_key.is_some())
            .field("tokens_present", &self.tokens.is_some())
            .field("last_refresh", &self.last_refresh)
            .field("agent_identity_present", &self.agent_identity.is_some())
            .field("additional_field_count", &self.additional_fields.len())
            .finish()
    }
}

impl std::fmt::Debug for ChatGptTokens {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatGptTokens")
            .field("id_token", &"[REDACTED]")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("account_id_present", &self.account_id.is_some())
            .field("additional_field_count", &self.additional_fields.len())
            .finish()
    }
}

impl std::fmt::Debug for IdTokenInfo {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IdTokenInfo")
            .field("raw_jwt", &"[REDACTED]")
            .field("email_present", &self.email.is_some())
            .field("plan_type_present", &self.chatgpt_plan_type.is_some())
            .field("user_id_present", &self.chatgpt_user_id.is_some())
            .field("account_id_present", &self.chatgpt_account_id.is_some())
            .finish()
    }
}

impl std::fmt::Debug for CodexAuth {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ChatGpt(_) => formatter
                .debug_tuple("ChatGpt")
                .field(&"[REDACTED]")
                .finish(),
            Self::ApiKey(_) => formatter
                .debug_tuple("ApiKey")
                .field(&"[REDACTED]")
                .finish(),
        }
    }
}

impl CodexAuth {
    /// Creates API-key credentials for tests.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn from_api_key(api_key: &str) -> Self {
        Self::ApiKey(api_key.to_string())
    }

    /// Creates deterministic `ChatGPT` credentials for tests.
    #[must_use]
    #[cfg(test)]
    pub(crate) fn create_dummy_chatgpt_auth_for_testing() -> Self {
        let tokens = ChatGptTokens {
            id_token: IdTokenInfo::create_for_testing("account_id"),
            access_token: "Access Token".to_string(),
            refresh_token: "Refresh Token".to_string(),
            account_id: Some("account_id".to_string()),
            additional_fields: BTreeMap::new(),
        };
        Self::ChatGpt(Box::new(AuthDotJson::from_tokens(tokens)))
    }

    /// Returns the bearer token to use in the Authorization header.
    ///
    /// # Errors
    ///
    /// Returns an error when no bearer token is present.
    pub fn get_token(&self) -> Result<&str, AuthError> {
        match self {
            Self::ApiKey(key) => Ok(key),
            Self::ChatGpt(auth) => auth
                .tokens
                .as_ref()
                .map(|tokens| tokens.access_token.as_str())
                .ok_or(AuthError::MissingToken),
        }
    }

    /// Returns the `ChatGPT` account id, when available.
    #[must_use]
    pub fn get_account_id(&self) -> Option<&str> {
        match self {
            Self::ApiKey(_) => None,
            Self::ChatGpt(auth) => auth
                .tokens
                .as_ref()
                .and_then(|tokens| tokens.account_id.as_deref()),
        }
    }
}

/// Credential extraction error.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The credential did not include a bearer token.
    #[error("missing bearer token")]
    MissingToken,
}

mod id_token_serde {
    use serde::{Deserialize, Deserializer, de::Error as _};

    use super::IdTokenInfo;

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<IdTokenInfo, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        IdTokenInfo::from_raw_jwt(raw).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod security_tests {
    use base64::Engine as _;

    use super::*;

    fn fixture_jwt(claims: &serde_json::Value) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"none","typ":"JWT"}"#);
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("{header}.{claims}.")
    }

    fn sentinel_tokens() -> ChatGptTokens {
        ChatGptTokens {
            id_token: IdTokenInfo {
                raw_jwt: "id-token-secret".to_owned(),
                email: Some("private@example.com".to_owned()),
                chatgpt_plan_type: Some("private-plan".to_owned()),
                chatgpt_user_id: Some("user-secret".to_owned()),
                chatgpt_account_id: Some("claim-account-secret".to_owned()),
            },
            access_token: "access-token-secret".to_owned(),
            refresh_token: "refresh-token-secret".to_owned(),
            account_id: Some("account-secret".to_owned()),
            additional_fields: BTreeMap::from([(
                "extension-key-secret".to_owned(),
                serde_json::json!("extension-value-secret"),
            )]),
        }
    }

    #[test]
    fn credential_debug_is_structural_and_redacted() {
        let tokens = sentinel_tokens();
        let auth = AuthDotJson {
            auth_mode: Some("chatgpt".to_owned()),
            openai_api_key: Some("api-key-secret".to_owned()),
            tokens: Some(tokens.clone()),
            last_refresh: None,
            agent_identity: Some(serde_json::json!({"secret": "identity-secret"})),
            additional_fields: BTreeMap::from([(
                "top-extension-key-secret".to_owned(),
                serde_json::json!("top-extension-value-secret"),
            )]),
        };
        let rendered = format!(
            "{auth:?} {tokens:?} {:?} {:?}",
            tokens.id_token,
            CodexAuth::ChatGpt(Box::new(auth.clone()))
        );

        for secret in [
            "id-token-secret",
            "private@example.com",
            "private-plan",
            "user-secret",
            "claim-account-secret",
            "access-token-secret",
            "refresh-token-secret",
            "account-secret",
            "api-key-secret",
            "identity-secret",
            "extension-key-secret",
            "extension-value-secret",
            "top-extension-key-secret",
            "top-extension-value-secret",
        ] {
            assert!(!rendered.contains(secret));
        }
        assert!(rendered.contains("[REDACTED]"));
    }

    #[test]
    fn direct_api_key_debug_is_redacted() {
        let rendered = format!("{:?}", CodexAuth::ApiKey("direct-key-secret".to_owned()));
        assert!(!rendered.contains("direct-key-secret"));
        assert!(rendered.contains("[REDACTED]"));
    }

    #[test]
    fn namespaced_account_claim_reaches_final_auth_account_id() -> Result<(), serde_json::Error> {
        let id_token = fixture_jwt(&serde_json::json!({
            "email": "fixture@example.invalid",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-fixture-namespaced",
                "chatgpt_plan_type": "fixture-plan"
            }
        }));
        let auth = serde_json::from_value::<AuthDotJson>(serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": id_token,
                "access_token": "not-an-access-token",
                "refresh_token": "not-a-refresh-token"
            }
        }))?;
        let auth = CodexAuth::ChatGpt(Box::new(auth));

        assert_eq!(auth.get_account_id(), Some("account-fixture-namespaced"));
        Ok(())
    }

    #[test]
    fn malformed_id_token_is_an_observable_deserialization_error() -> Result<(), std::io::Error> {
        let result = serde_json::from_value::<AuthDotJson>(serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "not-a-jwt",
                "access_token": "not-an-access-token",
                "refresh_token": "not-a-refresh-token"
            }
        }));
        let Err(error) = result else {
            return Err(std::io::Error::other(
                "malformed id token unexpectedly produced credential metadata",
            ));
        };

        assert!(
            error
                .to_string()
                .contains("JWT is missing its claims segment")
        );
        Ok(())
    }

    #[test]
    fn stored_and_claimed_account_id_conflict_is_rejected_without_disclosure()
    -> Result<(), std::io::Error> {
        let id_token = fixture_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "claim-account-secret"
            }
        }));
        let result = serde_json::from_value::<AuthDotJson>(serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": id_token,
                "access_token": "not-an-access-token",
                "refresh_token": "not-a-refresh-token",
                "account_id": "stored-account-secret"
            }
        }));
        let Err(error) = result else {
            return Err(std::io::Error::other(
                "conflicting account identifiers unexpectedly produced credentials",
            ));
        };
        let rendered = error.to_string();

        assert!(rendered.contains("conflicting ChatGPT account identifiers"));
        assert!(!rendered.contains("claim-account-secret"));
        assert!(!rendered.contains("stored-account-secret"));
        Ok(())
    }
}

#[cfg(test)]
#[path = "types_validation_tests.rs"]
mod validation_tests;
