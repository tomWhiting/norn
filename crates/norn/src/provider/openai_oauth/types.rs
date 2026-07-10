//! Codex CLI-compatible auth data types.

use serde::{Deserialize, Serialize};

use super::jwt::{IdTokenClaims, parse_id_token_claims};

/// Top-level `auth.json` shape shared with the Codex CLI.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AuthDotJson {
    /// Authentication mode. `ChatGPT` OAuth files use `chatgpt`.
    pub auth_mode: Option<String>,
    /// API key slot used by older/direct-key modes.
    #[serde(rename = "OPENAI_API_KEY")]
    pub openai_api_key: Option<String>,
    /// OAuth tokens, when logged in via `ChatGPT`.
    pub tokens: Option<ChatGptTokens>,
    /// Last successful token refresh timestamp.
    pub last_refresh: Option<chrono::DateTime<chrono::Utc>>,
    /// Reserved identity field retained for serde compatibility.
    pub agent_identity: Option<serde_json::Value>,
}

/// Token bundle stored under `tokens` in `auth.json`.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChatGptTokens {
    /// Raw id-token JWT string.
    #[serde(with = "id_token_serde")]
    pub id_token: IdTokenInfo,
    /// Raw access-token JWT string.
    pub access_token: String,
    /// Opaque refresh token.
    pub refresh_token: String,
    /// Optional `ChatGPT` account id.
    pub account_id: Option<String>,
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
    /// Creates id-token info from a raw JWT, parsing optional metadata on a
    /// best-effort basis.
    #[must_use]
    pub fn from_raw_jwt(raw_jwt: String) -> Self {
        let claims =
            parse_id_token_claims(&raw_jwt).unwrap_or_else(|_err| IdTokenClaims::default());
        Self::from_claims(raw_jwt, claims)
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
        }
    }
}

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
    pub fn from_api_key(api_key: &str) -> Self {
        Self::ApiKey(api_key.to_string())
    }

    /// Creates deterministic `ChatGPT` credentials for tests.
    #[must_use]
    pub fn create_dummy_chatgpt_auth_for_testing() -> Self {
        let tokens = ChatGptTokens {
            id_token: IdTokenInfo::from_raw_jwt("Id Token".to_string()),
            access_token: "Access Token".to_string(),
            refresh_token: "Refresh Token".to_string(),
            account_id: Some("account_id".to_string()),
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
    use serde::{Deserialize, Deserializer, Serializer};

    use super::IdTokenInfo;

    pub(super) fn serialize<S>(value: &IdTokenInfo, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.raw_jwt)
    }

    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<IdTokenInfo, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(IdTokenInfo::from_raw_jwt(raw))
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;

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
}
