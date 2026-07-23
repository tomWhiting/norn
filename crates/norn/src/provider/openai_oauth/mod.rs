//! Self-contained `OpenAI` `ChatGPT` OAuth support.
//!
//! This module implements the subset of Codex-compatible OAuth used by norn:
//! PKCE browser login, Norn-owned credential storage, proactive refresh,
//! revocation, and status/JWT helpers.

mod account_catalog;
mod auth_root;
mod browser;
mod code_exchange;
mod credential_decode;
mod credential_lock_timing;
mod credential_recovery;
mod credential_revision;
mod credential_state;
mod credential_transaction;
mod credential_validation;
mod device_login;
mod endpoints;
pub mod jwt;
mod login_commit;
mod login_prompt;
mod login_server;
mod manager;
mod options;
mod pkce;
mod refresh;
mod revoke;
mod storage;
mod types;

#[cfg(all(test, unix, not(any(target_os = "redox", target_os = "espidf"))))]
mod foreign_home_test_support;

#[cfg(test)]
#[path = "oauth_chain_tests.rs"]
mod chain_tests;

pub(crate) use account_catalog::prepare_named_account_logout;
pub use account_catalog::{
    AccountAlias, AccountCatalogError, AccountLogoutTarget, AccountSummary,
    AllAccountLogoutReservation, DEFAULT_ACCOUNT_ALIAS, DefaultLoginReservation,
    NamedLoginPreparation, NamedLoginReservation, list_accounts, prepare_all_account_logout,
    prepare_default_login, prepare_named_login, resolve_account_root, use_account,
    validate_default_login_identity,
};
pub use auth_root::{NornAuthRoot, NornAuthRootError, NornAuthRootSource, resolve_norn_auth_root};
pub use credential_state::{
    CredentialInspectionError, LocalCredentialState, MalformedCredentialReason,
    RefreshCandidateReason, UnknownExpiryReason, evaluate_chatgpt_credential,
    inspect_file_credential,
};
#[cfg(test)]
pub(crate) use credential_transaction::CredentialTransaction;
pub(crate) use device_login::{DeviceLoginOptions, run_device_login_with_hooks};
pub use endpoints::CLIENT_ID;
#[cfg(test)]
pub(crate) use login_commit::persist_prepared_login;
pub use login_prompt::{LoginPrompt, LoginPromptError, LoginPromptPresenter};
pub use login_server::{
    LoginError, LoginServer, LoginStorageFailureKind, ServerOptions, run_login_server,
};
pub(crate) use manager::OAuthCredentialIdentity;
pub use manager::{AuthManager, AuthManagerBuildError, RefreshTokenError};
pub use options::OAuthHttpOptions;
pub use revoke::{
    LocalLogoutError, LogoutError, LogoutReport, RemoteRevokeOutcome, logout_with_revoke,
};
pub(crate) use revoke::{complete_prepared_logout, prepare_local_logout};
pub use storage::{
    AUTH_JSON_FILE, AuthCredentialsStoreMode, DeleteAuthOutcome, StorageError, load_auth_dot_json,
};
pub use types::{AuthDotJson, ChatGptTokens, CodexAuth, IdTokenInfo};
