//! Codex CLI-compatible auth data types.

use serde::{Deserialize, Serialize};

use super::jwt::{IdTokenClaims, parse_id_token_claims};

/// Top-level `auth.json` shape shared with the Codex CLI.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
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
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodexAuth {
    /// `ChatGPT` OAuth credentials.
    ChatGpt(Box<AuthDotJson>),
    /// Direct API key credentials, retained for test helper compatibility.
    ApiKey(String),
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
