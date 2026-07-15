//! Typed, non-disclosing decoding of file-backed OAuth credentials.

use serde_json::{Map, Value};

use super::credential_validation::{CredentialField, validate_credential_value};
use super::jwt::{JwtError, parse_id_token_claims, reconcile_required_account_ids};
use super::types::AuthDotJson;

/// Safe structural reason a present credential cannot be used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MalformedCredentialReason {
    /// The JSON document could not be decoded.
    InvalidJson,
    /// The document identifies a non-ChatGPT authentication mode.
    UnsupportedAuthMode,
    /// Reserved for a caller-defined provider mode that forbids mixed slots.
    /// Codex `chatgpt` files may legitimately contain both populated fields.
    MixedCredentialKinds,
    /// A present OAuth document did not contain its token bundle.
    MissingTokenBundle,
    /// The id token contains malformed claims or compact-JWT structure.
    MalformedIdTokenClaims,
    /// The access token contains unsafe surrounding whitespace.
    InvalidAccessToken,
    /// A present refresh token cannot be sent safely to the authority.
    InvalidRefreshToken,
    /// Neither the token claims nor the stored bundle identify an account.
    MissingAccountId,
    /// An account identifier cannot be placed safely in an HTTP header.
    InvalidAccountId,
    /// Stored and claimed account identifiers disagree.
    ConflictingAccountIds,
    /// A JWT-shaped access token contains malformed claims or structure.
    MalformedAccessTokenClaims,
    /// Neither a usable access token nor a usable refresh token was present.
    MissingUsableToken,
}

impl std::fmt::Display for MalformedCredentialReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::InvalidJson => "invalid JSON",
            Self::UnsupportedAuthMode => "unsupported authentication mode",
            Self::MixedCredentialKinds => "mixed credential kinds",
            Self::MissingTokenBundle => "missing token bundle",
            Self::MalformedIdTokenClaims => "malformed id-token claims",
            Self::InvalidAccessToken => "invalid access-token shape",
            Self::InvalidRefreshToken => "invalid refresh-token shape",
            Self::MissingAccountId => "missing account identity",
            Self::InvalidAccountId => "invalid account identity",
            Self::ConflictingAccountIds => "conflicting account identities",
            Self::MalformedAccessTokenClaims => "malformed access-token claims",
            Self::MissingUsableToken => "missing usable token",
        })
    }
}

impl std::error::Error for MalformedCredentialReason {}

/// Decode one raw `auth.json` document without interpreting serde error text.
pub(super) fn decode_auth_dot_json(raw: &[u8]) -> Result<AuthDotJson, MalformedCredentialReason> {
    let structure: Value =
        serde_json::from_slice(raw).map_err(|_error| MalformedCredentialReason::InvalidJson)?;
    validate_structure(&structure)?;
    serde_json::from_slice(raw).map_err(|_error| MalformedCredentialReason::InvalidJson)
}

fn validate_structure(structure: &Value) -> Result<(), MalformedCredentialReason> {
    let root = structure
        .as_object()
        .ok_or(MalformedCredentialReason::InvalidJson)?;
    let auth_mode = optional_string(root, "auth_mode")?;
    if auth_mode.is_some_and(|mode| mode != "chatgpt") {
        return Err(MalformedCredentialReason::UnsupportedAuthMode);
    }
    optional_string(root, "OPENAI_API_KEY")?;
    let tokens = match root.get("tokens") {
        None | Some(Value::Null) => {
            return Err(MalformedCredentialReason::MissingTokenBundle);
        }
        Some(Value::Object(tokens)) => tokens,
        Some(_) => return Err(MalformedCredentialReason::InvalidJson),
    };
    validate_token_bundle(tokens)
}

fn validate_token_bundle(tokens: &Map<String, Value>) -> Result<(), MalformedCredentialReason> {
    let id_token = required_string(tokens, "id_token")?;
    let access_token = optional_string(tokens, "access_token")?.unwrap_or_default();
    let refresh_token = optional_string(tokens, "refresh_token")?.unwrap_or_default();
    let stored_account = optional_string(tokens, "account_id")?;

    let claims = parse_id_token_claims(id_token).map_err(|error| map_identity_error(&error))?;
    reconcile_required_account_ids(stored_account.map(str::to_owned), claims.chatgpt_account_id)
        .map_err(|error| map_identity_error(&error))?;

    if access_token.is_empty() && refresh_token.is_empty() {
        return Err(MalformedCredentialReason::MissingUsableToken);
    }
    if !access_token.is_empty() {
        validate_credential_value(CredentialField::AccessToken, access_token)
            .map_err(|_error| MalformedCredentialReason::InvalidAccessToken)?;
    }
    if !refresh_token.is_empty() {
        validate_credential_value(CredentialField::RefreshToken, refresh_token)
            .map_err(|_error| MalformedCredentialReason::InvalidRefreshToken)?;
    }
    Ok(())
}

fn optional_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<Option<&'a str>, MalformedCredentialReason> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_) => Err(MalformedCredentialReason::InvalidJson),
    }
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a str, MalformedCredentialReason> {
    optional_string(object, field)?.ok_or(MalformedCredentialReason::InvalidJson)
}

fn map_identity_error(error: &JwtError) -> MalformedCredentialReason {
    match error {
        JwtError::MissingAccountId => MalformedCredentialReason::MissingAccountId,
        JwtError::InvalidAccountId => MalformedCredentialReason::InvalidAccountId,
        JwtError::ConflictingAccountIds => MalformedCredentialReason::ConflictingAccountIds,
        JwtError::MissingClaims
        | JwtError::InvalidStructure
        | JwtError::Base64(_)
        | JwtError::Json(_)
        | JwtError::InvalidExpiration
        | JwtError::InvalidUserId
        | JwtError::ConflictingUserIds => MalformedCredentialReason::MalformedIdTokenClaims,
    }
}

#[cfg(test)]
#[path = "credential_decode_tests.rs"]
mod tests;
