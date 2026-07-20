//! Cached OAuth credential manager with proactive refresh.

use std::sync::Arc;

use sha2::{Digest as _, Sha256};
use tokio::sync::Mutex;

use super::auth_root::NornAuthRoot;
use super::credential_decode::MalformedCredentialReason;
use super::endpoints::TOKEN_URL;
use super::options::OAuthHttpOptions;
use super::storage::{AuthCredentialsStoreMode, StorageError};
use super::types::{AuthDotJson, CodexAuth};
use crate::provider::{CredentialIdentity, startup_trace};

#[path = "manager_attempt.rs"]
mod attempt;
#[path = "manager_commit.rs"]
mod commit;
#[path = "manager_refresh.rs"]
mod refresh_flow;
#[path = "manager_registry.rs"]
mod registry;

use attempt::RefreshAttempt;

/// Refresh result classification expected by auth.rs.
#[derive(Clone, Debug, thiserror::Error)]
pub enum RefreshTokenError {
    /// Credential is dead; user must log in again.
    #[error("{0}")]
    Permanent(String),
    /// Failure proven to precede token acceptance; caller may retry later.
    #[error("{0}")]
    Transient(String),
    /// The authority rotated the credential, but its owner did not durably
    /// accept the new lineage.
    #[error("{0}")]
    Undurable(String),
    /// The credential changed while the operation was in flight.
    #[error("{0}")]
    Conflict(String),
    /// Dispatch or task termination left the authority outcome unknown, or a
    /// success response returned no lineage that can be accepted safely.
    #[error("{0}")]
    Indeterminate(String),
    /// Local or inter-process credential coordination could not complete.
    #[error("{0}")]
    Coordination(String),
}

#[derive(Clone, Eq, Hash, PartialEq)]
struct AccountIdentity {
    account_id: String,
    user_id: Option<String>,
}

impl AccountIdentity {
    fn from_auth(auth: &AuthDotJson) -> Option<Self> {
        let tokens = auth.tokens.as_ref()?;
        Some(Self {
            account_id: tokens.account_id.clone()?,
            user_id: tokens.id_token.chatgpt_user_id.clone(),
        })
    }

    #[cfg(test)]
    fn from_codex(auth: &CodexAuth) -> Option<Self> {
        match auth {
            CodexAuth::ChatGpt(auth) => Self::from_auth(auth),
            CodexAuth::ApiKey(_) => None,
        }
    }
}

impl std::fmt::Debug for AccountIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AccountIdentity([REDACTED])")
    }
}

#[derive(Clone, Eq)]
struct RefreshLineage([u8; 32]);

impl RefreshLineage {
    fn from_auth(auth: &AuthDotJson) -> Option<Self> {
        auth.tokens.as_ref().and_then(|tokens| {
            (!tokens.refresh_token.is_empty()).then(|| {
                let digest = Sha256::digest(tokens.refresh_token.as_bytes());
                Self(digest.into())
            })
        })
    }
}

impl PartialEq for RefreshLineage {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl std::fmt::Debug for RefreshLineage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RefreshLineage([REDACTED])")
    }
}

/// Failure to construct an [`AuthManager`]'s shared HTTP client.
///
/// Deterministic (TLS backend or client-configuration initialisation
/// failed); retrying the identical construction cannot succeed.
#[derive(Debug, thiserror::Error)]
pub enum AuthManagerBuildError {
    /// The bounded OAuth HTTP client could not be constructed.
    #[error("failed to build OAuth HTTP client: {reason}")]
    HttpClient {
        /// Description of the client-construction failure.
        reason: String,
    },
    /// The credential store existed but could not be loaded safely.
    #[error("failed to load OAuth credentials: {0}")]
    CredentialStorage(#[from] StorageError),
    /// Credential locking or revision inspection failed.
    #[error("failed to coordinate OAuth credentials: {reason}")]
    CredentialCoordination {
        /// Non-disclosing description of the coordination failure.
        reason: String,
    },
    /// A live manager already owns this storage identity with other options.
    #[error("a live OAuth manager already owns this credential with different options")]
    ConfigurationConflict,
    /// Raw credential bytes existed but were not a usable credential document.
    #[error("OAuth credential storage is unusable: {reason}")]
    MalformedCredential {
        /// Non-disclosing structural reason retained for typed callers.
        reason: MalformedCredentialReason,
    },
}

#[derive(Clone, Debug)]
enum CachedAuthState {
    Missing,
    Ready {
        auth: CodexAuth,
        revision: Option<super::credential_transaction::CredentialRevision>,
    },
    PendingPersistence {
        refreshed: Box<super::types::AuthDotJson>,
        expected_revision: Option<super::credential_transaction::CredentialRevision>,
        error: RefreshTokenError,
    },
    Indeterminate {
        observed_revision: Option<super::credential_transaction::CredentialRevision>,
        observed_lineage: RefreshLineage,
        error: RefreshTokenError,
    },
}

/// Small credential manager compatible with norn's OAuth auth-provider use.
///
/// Mock token-authority constructors are not part of the production API, even
/// when the legacy `test-utils` feature is enabled:
///
/// ```compile_fail
/// use norn::provider::openai_oauth::AuthManager;
/// let _ = AuthManager::shared_for_tests;
/// ```
///
/// ```compile_fail
/// use norn::provider::openai_oauth::AuthManager;
/// let _ = AuthManager::from_static_auth_with_token_url;
/// ```
#[derive(Debug)]
pub struct AuthManager {
    auth_root: Option<NornAuthRoot>,
    http: OAuthHttpOptions,
    /// Account observed when this manager was constructed. This never changes;
    /// a replacement account receives a distinct registry owner.
    account_identity: Option<AccountIdentity>,
    auth: Mutex<CachedAuthState>,
    /// The exact in-flight attempt joined by every concurrent caller.
    refresh_attempt: Mutex<Option<Arc<RefreshAttempt>>>,
    /// Token-endpoint URL the refresh token is exchanged against.
    ///
    /// Fixed to the compiled authority ([`TOKEN_URL`]) by every
    /// production constructor — the endpoint is intrinsic to the
    /// credential's authority, and making it configurable (e.g. via an
    /// environment variable) would create a refresh-token exfiltration
    /// vector. Tests inject a mock server URL through the test-gated
    /// constructors instead.
    token_url: String,
    /// Shared HTTP client every refresh exchange through this manager
    /// reuses (connection pool plus the configured
    /// [`OAuthHttpOptions::request_timeout`]), built once at
    /// construction instead of per refresh call.
    client: reqwest::Client,
}

/// Builds the manager's shared HTTP client with the configured
/// whole-request deadline.
fn build_client(http: OAuthHttpOptions) -> Result<reqwest::Client, AuthManagerBuildError> {
    crate::provider::http_client::build_bounded_client(http.request_timeout).map_err(|err| {
        AuthManagerBuildError::HttpClient {
            reason: err.to_string(),
        }
    })
}

fn map_transaction_build_error(
    error: super::credential_transaction::CredentialTransactionError,
) -> AuthManagerBuildError {
    match error {
        super::credential_transaction::CredentialTransactionError::Storage(error) => {
            AuthManagerBuildError::CredentialStorage(error)
        }
        error => AuthManagerBuildError::CredentialCoordination {
            reason: error.to_string(),
        },
    }
}

impl AuthManager {
    /// Stable opaque identity of the account pinned when this manager opened.
    pub(crate) fn credential_identity(&self) -> Option<CredentialIdentity> {
        self.account_identity.as_ref().map(|identity| {
            CredentialIdentity::from_oauth_principal(
                &identity.account_id,
                identity.user_id.as_deref(),
            )
        })
    }

    /// Creates a shared manager and loads cached credentials from disk.
    ///
    /// The file-backed credential directory must cross the validated
    /// [`NornAuthRoot`] boundary before this constructor can be called.
    ///
    /// # Errors
    ///
    /// Returns [`AuthManagerBuildError`] when the shared HTTP client cannot be
    /// constructed or the credential store cannot be loaded safely.
    pub async fn shared(
        auth_root: NornAuthRoot,
        mode: AuthCredentialsStoreMode,
        http: OAuthHttpOptions,
    ) -> Result<Arc<Self>, AuthManagerBuildError> {
        registry::shared(auth_root, mode, TOKEN_URL.to_string(), http).await
    }

    /// [`Self::shared`] with an injected token-endpoint URL and the
    /// documented default [`OAuthHttpOptions`].
    ///
    /// Test-gated so production builds physically cannot redirect the
    /// refresh exchange away from the compiled authority.
    ///
    /// # Errors
    ///
    /// Returns [`AuthManagerBuildError`] when the shared HTTP client
    /// cannot be constructed.
    #[cfg(test)]
    pub(crate) async fn shared_for_tests(
        auth_root: NornAuthRoot,
        mode: AuthCredentialsStoreMode,
        token_url: String,
    ) -> Result<Arc<Self>, AuthManagerBuildError> {
        registry::shared(auth_root, mode, token_url, OAuthHttpOptions::default()).await
    }

    #[cfg(test)]
    pub(crate) async fn shared_for_tests_with_options(
        auth_root: NornAuthRoot,
        mode: AuthCredentialsStoreMode,
        token_url: String,
        http: OAuthHttpOptions,
    ) -> Result<Arc<Self>, AuthManagerBuildError> {
        registry::shared(auth_root, mode, token_url, http).await
    }

    async fn construct_file(
        auth_root: NornAuthRoot,
        token_url: String,
        http: OAuthHttpOptions,
    ) -> Result<Arc<Self>, AuthManagerBuildError> {
        let lock_timing = http.credential_lock_timing().map_err(|error| {
            AuthManagerBuildError::CredentialCoordination {
                reason: error.to_string(),
            }
        })?;
        let client = build_client(http)?;
        let shared_started = startup_trace::start("oauth_auth_manager_shared_with_token_url_start");
        tokio::task::yield_now().await;
        startup_trace::elapsed("oauth_auth_manager_initial_yield_done", shared_started);

        let load_started = startup_trace::start("oauth_auth_manager_load_auth_start");
        let transaction_root = auth_root.clone();
        let (snapshot, recovery) = tokio::task::spawn_blocking(move || {
            let transaction = super::credential_transaction::CredentialTransaction::acquire(
                &transaction_root,
                lock_timing,
            )?;
            let snapshot = transaction.snapshot()?;
            let recovery = transaction.reconcile_refresh_recovery(&snapshot);
            Ok::<_, super::credential_transaction::CredentialTransactionError>((snapshot, recovery))
        })
        .await
        .map_err(|error| AuthManagerBuildError::CredentialCoordination {
            reason: format!("credential inspection task failed: {error}"),
        })?
        .map_err(map_transaction_build_error)?;
        let recovery = recovery.map_err(|error| AuthManagerBuildError::CredentialCoordination {
            reason: error.to_string(),
        })?;
        let (auth, account_identity) = match snapshot.document {
            super::credential_transaction::CredentialDocument::Missing => {
                (CachedAuthState::Missing, None)
            }
            super::credential_transaction::CredentialDocument::Parsed(auth) => {
                let account_identity = AccountIdentity::from_auth(&auth).ok_or(
                    AuthManagerBuildError::MalformedCredential {
                        reason: MalformedCredentialReason::MissingAccountId,
                    },
                )?;
                let state = if recovery
                    == super::credential_recovery::RecoveryReconciliation::RecoveryRequired
                {
                    let observed_lineage = RefreshLineage::from_auth(&auth).ok_or(
                        AuthManagerBuildError::MalformedCredential {
                            reason: MalformedCredentialReason::InvalidRefreshToken,
                        },
                    )?;
                    CachedAuthState::Indeterminate {
                        observed_revision: snapshot.revision,
                        observed_lineage,
                        error: RefreshTokenError::Indeterminate(
                            "OAuth refresh recovery is required before this credential can rotate"
                                .to_owned(),
                        ),
                    }
                } else {
                    CachedAuthState::Ready {
                        auth: CodexAuth::ChatGpt(auth),
                        revision: snapshot.revision,
                    }
                };
                (state, Some(account_identity))
            }
            super::credential_transaction::CredentialDocument::Malformed(reason) => {
                return Err(AuthManagerBuildError::MalformedCredential { reason });
            }
        };
        startup_trace::auth_manager_load_done(
            load_started,
            matches!(auth, CachedAuthState::Ready { .. }),
        );
        startup_trace::elapsed(
            "oauth_auth_manager_shared_with_token_url_done",
            shared_started,
        );
        Ok(Arc::new(Self {
            auth_root: Some(auth_root),
            http,
            account_identity,
            auth: Mutex::new(auth),
            refresh_attempt: Mutex::new(None),
            token_url,
            client,
        }))
    }

    /// Creates a shared manager seeded with an in-memory test credential.
    /// It can serve that credential, but cannot rotate it without a durable
    /// owner sink.
    ///
    /// # Errors
    ///
    /// Returns [`AuthManagerBuildError`] when the shared HTTP client
    /// cannot be constructed.
    #[cfg(test)]
    pub(crate) fn from_static_auth(
        auth: CodexAuth,
        http: OAuthHttpOptions,
    ) -> Result<Arc<Self>, AuthManagerBuildError> {
        Self::static_auth_with_token_url(auth, TOKEN_URL.to_string(), http)
    }

    /// [`Self::from_static_auth`] with an injected token-endpoint URL and
    /// the documented default [`OAuthHttpOptions`].
    ///
    /// Test-gated so production builds physically cannot redirect the
    /// refresh exchange away from the compiled authority.
    ///
    /// # Errors
    ///
    /// Returns [`AuthManagerBuildError`] when the shared HTTP client
    /// cannot be constructed.
    #[cfg(test)]
    pub(crate) fn from_static_auth_with_token_url(
        auth: CodexAuth,
        token_url: String,
    ) -> Result<Arc<Self>, AuthManagerBuildError> {
        Self::static_auth_with_token_url(auth, token_url, OAuthHttpOptions::default())
    }

    #[cfg(test)]
    fn static_auth_with_token_url(
        auth: CodexAuth,
        token_url: String,
        http: OAuthHttpOptions,
    ) -> Result<Arc<Self>, AuthManagerBuildError> {
        let client = build_client(http)?;
        let account_identity = AccountIdentity::from_codex(&auth);
        Ok(Arc::new(Self {
            auth_root: None,
            http,
            account_identity,
            auth: Mutex::new(CachedAuthState::Ready {
                auth,
                revision: None,
            }),
            refresh_attempt: Mutex::new(None),
            token_url,
            client,
        }))
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn manager_debug_redacts_cached_credentials() -> Result<(), AuthManagerBuildError> {
        let manager = AuthManager::from_static_auth(
            CodexAuth::from_api_key("manager-api-key-secret"),
            OAuthHttpOptions::default(),
        )?;

        let rendered = format!("{manager:?}");
        assert!(!rendered.contains("manager-api-key-secret"));
        assert!(rendered.contains("[REDACTED]"));
        Ok(())
    }

    #[test]
    fn malformed_credential_display_retains_the_typed_reason() {
        let error = AuthManagerBuildError::MalformedCredential {
            reason: MalformedCredentialReason::UnsupportedAuthMode,
        };
        let rendered = error.to_string();

        assert!(rendered.contains("unsupported authentication mode"));
        assert!(!rendered.contains("malformed JSON"));
    }

    #[tokio::test]
    async fn malformed_credential_storage_fails_manager_construction()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = tempfile::tempdir()?;
        std::fs::write(home.path().join("auth.json"), b"{malformed")?;
        let auth_root = NornAuthRoot::try_from(home.path())?;

        let result = AuthManager::shared_for_tests(
            auth_root,
            AuthCredentialsStoreMode::File,
            "http://127.0.0.1:9".to_owned(),
        )
        .await;

        assert!(matches!(
            result,
            Err(AuthManagerBuildError::MalformedCredential {
                reason: MalformedCredentialReason::InvalidJson,
            })
        ));
        Ok(())
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "manager_state_tests.rs"]
mod state_tests;

#[cfg(test)]
#[path = "manager_process_tests.rs"]
mod process_tests;

#[cfg(test)]
#[path = "manager_supervision_tests.rs"]
mod supervision_tests;

#[cfg(all(test, unix, not(any(target_os = "redox", target_os = "espidf"))))]
#[path = "manager_foreign_home_tests.rs"]
mod foreign_home_tests;
