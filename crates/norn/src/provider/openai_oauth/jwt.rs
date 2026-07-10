//! JWT parsing helpers for OAuth token metadata.

use base64::Engine as _;
use chrono::{DateTime, TimeZone, Utc};
use serde::Deserialize;

/// Errors that can occur while reading unverified JWT claims.
#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    /// JWT did not contain the expected three dot-separated segments.
    #[error("JWT is missing its claims segment")]
    MissingClaims,
    /// Claims segment was not valid base64url data.
    #[error("JWT claims are not valid base64url: {0}")]
    Base64(#[from] base64::DecodeError),
    /// Claims segment was not valid JSON.
    #[error("JWT claims are not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The exp claim was absent or not a valid timestamp.
    #[error("JWT does not contain a valid exp claim")]
    MissingExpiration,
}

#[derive(Debug, Deserialize)]
struct ExpClaims {
    exp: i64,
}

/// Unverified claims `OpenAI` includes in the `ChatGPT` id token.
#[derive(Clone, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct IdTokenClaims {
    /// User email address, when present.
    pub email: Option<String>,
    /// `ChatGPT` subscription plan type, when present.
    pub chatgpt_plan_type: Option<String>,
    /// `ChatGPT` user id, when present.
    pub chatgpt_user_id: Option<String>,
    /// `ChatGPT` account id, when present.
    pub chatgpt_account_id: Option<String>,
}

impl std::fmt::Debug for IdTokenClaims {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("IdTokenClaims")
            .field("email_present", &self.email.is_some())
            .field("plan_type_present", &self.chatgpt_plan_type.is_some())
            .field("user_id_present", &self.chatgpt_user_id.is_some())
            .field("account_id_present", &self.chatgpt_account_id.is_some())
            .finish()
    }
}

/// Parses the `exp` claim from an access-token JWT without verifying the
/// signature.
///
/// # Errors
///
/// Returns [`JwtError`] if the JWT cannot be decoded or lacks a valid `exp`.
pub fn parse_jwt_expiration(jwt: &str) -> Result<Option<DateTime<Utc>>, JwtError> {
    let claims = decode_claims::<ExpClaims>(jwt)?;
    let Some(dt) = Utc.timestamp_opt(claims.exp, 0).single() else {
        return Err(JwtError::MissingExpiration);
    };
    Ok(Some(dt))
}

/// Parses selected id-token metadata while preserving the raw JWT elsewhere.
///
/// # Errors
///
/// Returns [`JwtError`] when the JWT claims segment is malformed.
pub fn parse_id_token_claims(jwt: &str) -> Result<IdTokenClaims, JwtError> {
    decode_claims(jwt)
}

fn decode_claims<T>(jwt: &str) -> Result<T, JwtError>
where
    T: for<'de> Deserialize<'de>,
{
    let claims_segment = jwt.split('.').nth(1).ok_or(JwtError::MissingClaims)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(claims_segment)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn id_token_claims_debug_is_presence_only() {
        let claims = IdTokenClaims {
            email: Some("private@example.com".to_owned()),
            chatgpt_plan_type: Some("private-plan".to_owned()),
            chatgpt_user_id: Some("user-secret".to_owned()),
            chatgpt_account_id: Some("account-secret".to_owned()),
        };

        let rendered = format!("{claims:?}");
        for secret in [
            "private@example.com",
            "private-plan",
            "user-secret",
            "account-secret",
        ] {
            assert!(!rendered.contains(secret));
        }
        assert!(rendered.contains("email_present"));
        assert!(rendered.contains("account_id_present"));
    }
}
