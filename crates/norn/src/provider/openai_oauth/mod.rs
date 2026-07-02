//! Self-contained `OpenAI` `ChatGPT` OAuth support.
//!
//! This module implements the subset of Codex CLI-compatible OAuth used by
//! norn: PKCE browser login, `~/.codex/auth.json` storage, proactive refresh,
//! revocation, and status/JWT helpers.

pub mod jwt;
mod login_server;
mod manager;
mod options;
mod pkce;
mod refresh;
mod revoke;
mod storage;
mod types;

pub use login_server::{LoginServer, ServerOptions, run_login_server};
pub use manager::{AuthManager, AuthManagerBuildError, RefreshTokenError};
pub use options::OAuthHttpOptions;
pub use revoke::logout_with_revoke;
pub use storage::{AUTH_JSON_FILE, AuthCredentialsStoreMode, load_auth_dot_json};
pub use types::{AuthDotJson, ChatGptTokens, CodexAuth, IdTokenInfo};

/// Public `OpenAI` OAuth client id used by Codex CLI-compatible auth.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const REVOKE_URL: &str = "https://auth.openai.com/oauth/revoke";
const OAUTH_SCOPES: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
