//! JWT parsing helpers for OAuth token metadata.

use base64::Engine as _;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Deserializer, de::Error as _};

use super::credential_validation::{CredentialField, validate_credential_value};

/// Errors that can occur while reading unverified JWT claims.
#[derive(Debug, thiserror::Error)]
pub enum JwtError {
    /// JWT did not contain the expected three dot-separated segments.
    #[error("JWT is missing its claims segment")]
    MissingClaims,
    /// A JWT-shaped value did not contain exactly three compact segments.
    #[error("JWT does not have a valid compact structure")]
    InvalidStructure,
    /// A compact JWT segment was not valid base64url data.
    #[error("JWT contains invalid base64url data: {0}")]
    Base64(#[from] base64::DecodeError),
    /// The header or claims segment was not valid JSON for its expected shape.
    #[error("JWT contains invalid JSON metadata: {0}")]
    Json(#[from] serde_json::Error),
    /// The exp claim was outside the supported timestamp range.
    #[error("JWT exp claim is not a valid timestamp")]
    InvalidExpiration,
    /// Neither supported claim source supplied an account id.
    #[error("ChatGPT credentials are missing an account identifier")]
    MissingAccountId,
    /// A supported claim source supplied an unsafe account id.
    #[error("ChatGPT credentials contain an invalid account identifier")]
    InvalidAccountId,
    /// Two supported claim sources named different nonempty accounts.
    #[error("JWT contains conflicting ChatGPT account identifiers")]
    ConflictingAccountIds,
    /// A supported claim source supplied an unsafe user id.
    #[error("ChatGPT credentials contain an invalid user identifier")]
    InvalidUserId,
    /// Two supported claim sources named different nonempty users.
    #[error("JWT contains conflicting ChatGPT user identifiers")]
    ConflictingUserIds,
}

#[derive(Debug, Deserialize)]
struct ExpClaims {
    #[serde(default)]
    exp: ExpClaim,
}

#[derive(Debug, Default)]
struct ExpClaim(Option<i64>);

impl<'de> Deserialize<'de> for ExpClaim {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        i64::deserialize(deserializer).map(|value| Self(Some(value)))
    }
}

/// Unverified claims `OpenAI` includes in the `ChatGPT` id token.
#[derive(Clone, Default, PartialEq, Eq)]
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

impl<'de> Deserialize<'de> for IdTokenClaims {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawIdTokenClaims::deserialize(deserializer)?;
        normalize_id_token_claims(raw).map_err(D::Error::custom)
    }
}

#[derive(Deserialize)]
struct RawIdTokenClaims {
    email: Option<String>,
    chatgpt_plan_type: Option<String>,
    chatgpt_user_id: Option<String>,
    chatgpt_account_id: Option<String>,
    #[serde(rename = "https://api.openai.com/auth")]
    openai_auth: Option<OpenAiAuthClaims>,
}

#[derive(Default, Deserialize)]
struct OpenAiAuthClaims {
    chatgpt_plan_type: Option<String>,
    chatgpt_user_id: Option<String>,
    user_id: Option<String>,
    chatgpt_account_id: Option<String>,
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
/// Returns [`JwtError`] if the JWT cannot be decoded or contains an invalid
/// `exp`. A valid claims object without `exp` returns `Ok(None)`.
pub fn parse_jwt_expiration(jwt: &str) -> Result<Option<DateTime<Utc>>, JwtError> {
    let claims = decode_claims::<ExpClaims>(jwt)?;
    let Some(expiration) = claims.exp.0 else {
        return Ok(None);
    };
    let Some(timestamp) = Utc.timestamp_opt(expiration, 0).single() else {
        return Err(JwtError::InvalidExpiration);
    };
    Ok(Some(timestamp))
}

/// Parses selected id-token metadata while preserving the raw JWT elsewhere.
///
/// # Errors
///
/// Returns [`JwtError`] when JWT metadata is malformed or a supported account
/// or user identity source is invalid or conflicts with another source.
pub fn parse_id_token_claims(jwt: &str) -> Result<IdTokenClaims, JwtError> {
    let raw = decode_claims::<RawIdTokenClaims>(jwt)?;
    normalize_id_token_claims(raw)
}

fn normalize_id_token_claims(raw: RawIdTokenClaims) -> Result<IdTokenClaims, JwtError> {
    let namespaced = raw.openai_auth.unwrap_or_default();
    let chatgpt_account_id =
        reconcile_account_ids(namespaced.chatgpt_account_id, raw.chatgpt_account_id)?;
    let namespaced_user_id = reconcile_user_ids(namespaced.chatgpt_user_id, namespaced.user_id)?;
    let chatgpt_user_id = reconcile_user_ids(namespaced_user_id, raw.chatgpt_user_id)?;

    Ok(IdTokenClaims {
        email: raw.email,
        chatgpt_plan_type: namespaced.chatgpt_plan_type.or(raw.chatgpt_plan_type),
        chatgpt_user_id,
        chatgpt_account_id,
    })
}

fn reconcile_user_ids(
    preferred: Option<String>,
    fallback: Option<String>,
) -> Result<Option<String>, JwtError> {
    for value in [preferred.as_deref(), fallback.as_deref()]
        .into_iter()
        .flatten()
    {
        if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
            return Err(JwtError::InvalidUserId);
        }
    }

    match (preferred, fallback) {
        (Some(preferred), Some(fallback)) if preferred != fallback => {
            Err(JwtError::ConflictingUserIds)
        }
        (Some(preferred), _) => Ok(Some(preferred)),
        (None, fallback) => Ok(fallback),
    }
}

/// Reconciles a preferred account id with a compatibility fallback.
///
/// # Errors
///
/// Returns [`JwtError`] when a present identity is invalid or the sources
/// conflict.
pub(super) fn reconcile_account_ids(
    preferred: Option<String>,
    fallback: Option<String>,
) -> Result<Option<String>, JwtError> {
    for value in [preferred.as_deref(), fallback.as_deref()]
        .into_iter()
        .flatten()
    {
        if validate_credential_value(CredentialField::AccountId, value).is_err() {
            return Err(JwtError::InvalidAccountId);
        }
    }

    match (preferred, fallback) {
        (Some(preferred), Some(fallback)) if preferred != fallback => {
            Err(JwtError::ConflictingAccountIds)
        }
        (Some(preferred), _) => Ok(Some(preferred)),
        (None, fallback) => Ok(fallback),
    }
}

/// Reconciles account-id sources and requires one usable identity.
///
/// # Errors
///
/// Returns [`JwtError`] when an identity is missing, invalid, or conflicting.
pub(super) fn reconcile_required_account_ids(
    preferred: Option<String>,
    fallback: Option<String>,
) -> Result<String, JwtError> {
    reconcile_account_ids(preferred, fallback)?.ok_or(JwtError::MissingAccountId)
}

fn decode_claims<T>(jwt: &str) -> Result<T, JwtError>
where
    T: for<'de> Deserialize<'de>,
{
    if !jwt.contains('.') {
        return Err(JwtError::MissingClaims);
    }
    let mut segments = jwt.split('.');
    let header_segment = segments.next().ok_or(JwtError::InvalidStructure)?;
    let claims_segment = segments.next().ok_or(JwtError::InvalidStructure)?;
    let signature_segment = segments.next().ok_or(JwtError::InvalidStructure)?;
    if segments.next().is_some() || header_segment.is_empty() || claims_segment.is_empty() {
        return Err(JwtError::InvalidStructure);
    }
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(header_segment)?;
    let _: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&header)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(claims_segment)?;
    let claims = serde_json::from_slice(&bytes)?;
    if !signature_segment.is_empty() {
        drop(base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(signature_segment)?);
    }
    Ok(claims)
}

#[cfg(test)]
mod security_tests {
    use super::*;

    fn fixture_jwt(claims: &serde_json::Value) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"none","typ":"JWT"}"#);
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("{header}.{claims}.")
    }

    #[test]
    fn namespaced_codex_claims_are_parsed() -> Result<(), JwtError> {
        let jwt = fixture_jwt(&serde_json::json!({
            "email": "fixture@example.invalid",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-fixture-namespaced",
                "chatgpt_plan_type": "fixture-plan",
                "chatgpt_user_id": "user-fixture-namespaced"
            }
        }));

        let claims = parse_id_token_claims(&jwt)?;

        assert_eq!(claims.email.as_deref(), Some("fixture@example.invalid"));
        assert_eq!(
            claims.chatgpt_account_id.as_deref(),
            Some("account-fixture-namespaced")
        );
        assert_eq!(claims.chatgpt_plan_type.as_deref(), Some("fixture-plan"));
        assert_eq!(
            claims.chatgpt_user_id.as_deref(),
            Some("user-fixture-namespaced")
        );
        Ok(())
    }

    #[test]
    fn namespaced_legacy_user_id_is_a_supported_fallback() -> Result<(), JwtError> {
        let jwt = fixture_jwt(&serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-fixture",
                "user_id": "user-fixture-legacy"
            }
        }));

        assert_eq!(
            parse_id_token_claims(&jwt)?.chatgpt_user_id.as_deref(),
            Some("user-fixture-legacy")
        );
        Ok(())
    }

    #[test]
    fn direct_claim_deserialization_uses_the_same_normalization() -> Result<(), serde_json::Error> {
        let claims = serde_json::from_value::<IdTokenClaims>(serde_json::json!({
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-fixture-namespaced"
            }
        }))?;

        assert_eq!(
            claims.chatgpt_account_id.as_deref(),
            Some("account-fixture-namespaced")
        );
        Ok(())
    }

    #[test]
    fn flat_codex_claims_remain_a_supported_fallback() -> Result<(), JwtError> {
        let jwt = fixture_jwt(&serde_json::json!({
            "email": "fixture@example.invalid",
            "chatgpt_account_id": "account-fixture-flat",
            "chatgpt_plan_type": "fixture-flat-plan",
            "chatgpt_user_id": "user-fixture-flat"
        }));

        let claims = parse_id_token_claims(&jwt)?;

        assert_eq!(
            claims.chatgpt_account_id.as_deref(),
            Some("account-fixture-flat")
        );
        assert_eq!(
            claims.chatgpt_plan_type.as_deref(),
            Some("fixture-flat-plan")
        );
        assert_eq!(claims.chatgpt_user_id.as_deref(), Some("user-fixture-flat"));
        Ok(())
    }

    #[test]
    fn conflicting_account_claims_are_rejected() {
        let jwt = fixture_jwt(&serde_json::json!({
            "chatgpt_account_id": "account-fixture-flat",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-fixture-namespaced"
            }
        }));

        assert!(matches!(
            parse_id_token_claims(&jwt),
            Err(JwtError::ConflictingAccountIds)
        ));
    }

    #[test]
    fn conflicting_user_claims_are_rejected() {
        for claims in [
            serde_json::json!({
                "https://api.openai.com/auth": {
                    "chatgpt_user_id": "user-primary",
                    "user_id": "user-legacy"
                }
            }),
            serde_json::json!({
                "chatgpt_user_id": "user-flat",
                "https://api.openai.com/auth": {
                    "chatgpt_user_id": "user-namespaced"
                }
            }),
        ] {
            let jwt = fixture_jwt(&claims);
            assert!(matches!(
                parse_id_token_claims(&jwt),
                Err(JwtError::ConflictingUserIds)
            ));
        }
    }

    #[test]
    fn unsafe_user_claims_are_rejected() {
        for user_id in ["", " user", "user\rsecret", "user\0secret"] {
            let jwt = fixture_jwt(&serde_json::json!({
                "https://api.openai.com/auth": {
                    "chatgpt_user_id": user_id
                }
            }));
            assert!(matches!(
                parse_id_token_claims(&jwt),
                Err(JwtError::InvalidUserId)
            ));
        }
    }

    #[test]
    fn equal_account_claims_are_not_conflicts() -> Result<(), JwtError> {
        let equal = fixture_jwt(&serde_json::json!({
            "chatgpt_account_id": "account-fixture",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "account-fixture"
            }
        }));

        assert_eq!(
            parse_id_token_claims(&equal)?.chatgpt_account_id.as_deref(),
            Some("account-fixture")
        );
        Ok(())
    }

    #[test]
    fn empty_or_control_bearing_account_claims_are_rejected() {
        for account_id in ["", " account", "account\rsecret", "account\0secret"] {
            let jwt = fixture_jwt(&serde_json::json!({
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": account_id
                }
            }));

            assert!(matches!(
                parse_id_token_claims(&jwt),
                Err(JwtError::InvalidAccountId)
            ));
        }
    }

    #[test]
    fn missing_account_is_typed_when_credentials_require_identity() {
        assert!(matches!(
            reconcile_required_account_ids(None, None),
            Err(JwtError::MissingAccountId)
        ));
    }

    #[test]
    fn expiration_distinguishes_absent_and_malformed_claims() -> Result<(), JwtError> {
        let absent = fixture_jwt(&serde_json::json!({"scope": "openid"}));

        assert_eq!(parse_jwt_expiration(&absent)?, None);
        for claims in [
            serde_json::json!({"exp": "not-a-timestamp"}),
            serde_json::json!({"exp": null}),
            serde_json::json!({"exp": []}),
        ] {
            let malformed = fixture_jwt(&claims);
            assert!(matches!(
                parse_jwt_expiration(&malformed),
                Err(JwtError::Json(_))
            ));
        }
        let out_of_range = fixture_jwt(&serde_json::json!({"exp": i64::MAX}));
        assert!(matches!(
            parse_jwt_expiration(&out_of_range),
            Err(JwtError::InvalidExpiration)
        ));
        Ok(())
    }

    #[test]
    fn jwt_shaped_values_with_invalid_segment_counts_are_not_opaque() {
        for value in ["header.claims", "header.claims.signature.extra", ".claims."] {
            assert!(matches!(
                parse_jwt_expiration(value),
                Err(JwtError::InvalidStructure)
            ));
        }
        assert!(matches!(
            parse_jwt_expiration("opaque-token"),
            Err(JwtError::MissingClaims)
        ));
    }

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
