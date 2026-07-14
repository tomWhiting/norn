//! Self-contained `OpenAI` `ChatGPT` OAuth support.
//!
//! This module implements the subset of Codex CLI-compatible OAuth used by
//! norn: PKCE browser login, `~/.codex/auth.json` storage, proactive refresh,
//! revocation, and status/JWT helpers.

mod browser;
mod code_exchange;
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

pub use endpoints::CLIENT_ID;
pub use login_server::{LoginServer, ServerOptions, run_login_server};
pub use manager::{AuthManager, AuthManagerBuildError, RefreshTokenError};
pub use options::OAuthHttpOptions;
pub use revoke::logout_with_revoke;
pub use storage::{AUTH_JSON_FILE, AuthCredentialsStoreMode, load_auth_dot_json};
pub use types::{AuthDotJson, ChatGptTokens, CodexAuth, IdTokenInfo};

use endpoints::{AUTHORIZE_URL, OAUTH_SCOPES, REVOKE_URL, TOKEN_URL};
