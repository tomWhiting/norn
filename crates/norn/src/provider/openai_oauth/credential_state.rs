//! Side-effect-free local classification of file-backed OAuth credentials.

use chrono::{DateTime, Utc};

use super::auth_root::NornAuthRoot;
pub use super::credential_decode::MalformedCredentialReason;
use super::credential_validation::{CredentialField, validate_credential_value};
use super::jwt::{JwtError, parse_jwt_expiration};
use super::storage::{AuthCredentialsStoreMode, StorageError, load_auth_dot_json_observational};
use super::types::AuthDotJson;

/// Why a credential must refresh before a provider request can start.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshCandidateReason {
    /// The access token's expiry is at or before the evaluation time.
    AccessExpired,
    /// The access token is absent but a refresh token is available.
    AccessMissing,
}

/// Why local inspection cannot determine the access-token expiry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnknownExpiryReason {
    /// The access token is opaque rather than a three-segment JWT.
    OpaqueAccessToken,
    /// Valid JWT claims did not contain an expiration timestamp.
    MissingExpiration,
}

/// Local credential state. This never claims remote validity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalCredentialState {
    /// No credential file exists.
    Missing,
    /// A file exists but its structure cannot be used safely.
    Malformed {
        /// Non-disclosing structural reason.
        reason: MalformedCredentialReason,
    },
    /// The access token is expired and there is no usable refresh path.
    AccessExpired {
        /// Timestamp read from the unverified JWT claims.
        expired_at: DateTime<Utc>,
    },
    /// A refresh is required before the next provider request.
    RefreshCandidate {
        /// Why refresh is required.
        reason: RefreshCandidateReason,
        /// Known expiry, when the access token carried one.
        expired_at: Option<DateTime<Utc>>,
    },
    /// The access token has a known future expiry.
    LocallyValid {
        /// Timestamp read from the unverified JWT claims.
        expires_at: DateTime<Utc>,
    },
    /// A nonempty access token exists, but its expiry is not locally knowable.
    Unknown {
        /// Non-disclosing reason local expiry inspection was inconclusive.
        reason: UnknownExpiryReason,
    },
}

/// I/O failure while inspecting a file-backed credential.
#[derive(Debug, thiserror::Error)]
pub enum CredentialInspectionError {
    /// Credential storage could not be read safely.
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Load and classify a file-backed credential from a validated Norn-owned root
/// without refreshing it.
///
/// # Errors
///
/// Returns [`CredentialInspectionError`] for I/O, permission, symlink, and
/// non-regular-file failures. Malformed credentials are a local state rather
/// than an I/O failure.
pub fn inspect_file_credential(
    auth_root: &NornAuthRoot,
    mode: AuthCredentialsStoreMode,
    now: DateTime<Utc>,
) -> Result<LocalCredentialState, CredentialInspectionError> {
    match load_auth_dot_json_observational(auth_root, mode) {
        Ok(Some(auth)) => Ok(evaluate_chatgpt_credential(&auth, now)),
        Ok(None) => Ok(LocalCredentialState::Missing),
        Err(StorageError::MalformedCredential(reason)) => {
            Ok(LocalCredentialState::Malformed { reason })
        }
        Err(StorageError::Json(_)) => Ok(LocalCredentialState::Malformed {
            reason: MalformedCredentialReason::InvalidJson,
        }),
        Err(error) => Err(error.into()),
    }
}

/// Classify one decoded `auth.json` document at a caller-supplied time.
#[must_use]
pub fn evaluate_chatgpt_credential(auth: &AuthDotJson, now: DateTime<Utc>) -> LocalCredentialState {
    if auth
        .auth_mode
        .as_deref()
        .is_some_and(|mode| mode != "chatgpt")
    {
        return LocalCredentialState::Malformed {
            reason: MalformedCredentialReason::UnsupportedAuthMode,
        };
    }
    let Some(tokens) = auth.tokens.as_ref() else {
        return LocalCredentialState::Malformed {
            reason: MalformedCredentialReason::MissingTokenBundle,
        };
    };
    if let Err(error) = tokens
        .id_token
        .reconcile_account_id(tokens.account_id.clone())
    {
        let reason = match error {
            JwtError::MissingAccountId => MalformedCredentialReason::MissingAccountId,
            JwtError::ConflictingAccountIds => MalformedCredentialReason::ConflictingAccountIds,
            _ => MalformedCredentialReason::InvalidAccountId,
        };
        return LocalCredentialState::Malformed { reason };
    }

    let refresh_usable = if tokens.refresh_token.is_empty() {
        false
    } else if validate_credential_value(CredentialField::RefreshToken, &tokens.refresh_token)
        .is_err()
    {
        return LocalCredentialState::Malformed {
            reason: MalformedCredentialReason::InvalidRefreshToken,
        };
    } else {
        true
    };
    if tokens.access_token.is_empty() {
        return if refresh_usable {
            LocalCredentialState::RefreshCandidate {
                reason: RefreshCandidateReason::AccessMissing,
                expired_at: None,
            }
        } else {
            LocalCredentialState::Malformed {
                reason: MalformedCredentialReason::MissingUsableToken,
            }
        };
    }
    if validate_credential_value(CredentialField::AccessToken, &tokens.access_token).is_err() {
        return LocalCredentialState::Malformed {
            reason: MalformedCredentialReason::InvalidAccessToken,
        };
    }

    match parse_jwt_expiration(&tokens.access_token) {
        Ok(Some(expires_at)) if expires_at > now => {
            LocalCredentialState::LocallyValid { expires_at }
        }
        Ok(Some(expired_at)) if refresh_usable => LocalCredentialState::RefreshCandidate {
            reason: RefreshCandidateReason::AccessExpired,
            expired_at: Some(expired_at),
        },
        Ok(Some(expired_at)) => LocalCredentialState::AccessExpired { expired_at },
        Ok(None) => LocalCredentialState::Unknown {
            reason: UnknownExpiryReason::MissingExpiration,
        },
        Err(JwtError::MissingClaims) => LocalCredentialState::Unknown {
            reason: UnknownExpiryReason::OpaqueAccessToken,
        },
        Err(_) => LocalCredentialState::Malformed {
            reason: MalformedCredentialReason::MalformedAccessTokenClaims,
        },
    }
}

#[cfg(test)]
#[path = "credential_state_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "credential_state_security_tests.rs"]
mod security_tests;
