//! Self-contained `OpenAI` `ChatGPT` OAuth support.
//!
//! This module implements the subset of Codex-compatible OAuth used by norn:
//! PKCE browser login, Norn-owned credential storage, proactive refresh,
//! revocation, and status/JWT helpers.

mod auth_root;
mod browser;
mod code_exchange;
mod credential_decode;
mod credential_lock_timing;
mod credential_state;
mod credential_transaction;
mod credential_validation;
mod endpoints;
pub mod jwt;
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

pub use auth_root::{NornAuthRoot, NornAuthRootError, NornAuthRootSource, resolve_norn_auth_root};
pub use credential_state::{
    CredentialInspectionError, LocalCredentialState, MalformedCredentialReason,
    RefreshCandidateReason, UnknownExpiryReason, evaluate_chatgpt_credential,
    inspect_file_credential,
};
pub use endpoints::CLIENT_ID;
pub use login_server::{
    LoginError, LoginServer, LoginStorageFailureKind, ServerOptions, run_login_server,
};
pub use manager::{AuthManager, AuthManagerBuildError, RefreshTokenError};
pub use options::OAuthHttpOptions;
pub use revoke::{
    LocalLogoutError, LogoutError, LogoutReport, RemoteRevokeOutcome, logout_with_revoke,
};
pub use storage::{
    AUTH_JSON_FILE, AuthCredentialsStoreMode, DeleteAuthOutcome, StorageError, load_auth_dot_json,
};
pub use types::{AuthDotJson, ChatGptTokens, CodexAuth, IdTokenInfo};
