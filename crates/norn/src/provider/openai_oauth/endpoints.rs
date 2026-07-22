//! Compiled `OpenAI` OAuth authority endpoints and client identity.
//!
//! These are the fixed Codex CLI-compatible values the OAuth flow authenticates
//! against. They are re-exported from the module root so sibling modules refer
//! to them as `super::TOKEN_URL` (etc.) and [`CLIENT_ID`] keeps its public path.

/// Public `OpenAI` OAuth client id used by Codex CLI-compatible auth.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// Authorization endpoint for the PKCE browser login.
pub(super) const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";

/// Device-code request endpoint used by headless login.
pub(super) const DEVICE_USER_CODE_URL: &str =
    "https://auth.openai.com/api/accounts/deviceauth/usercode";

/// Device-code polling endpoint used by headless login.
pub(super) const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";

/// Human verification page used by headless login.
pub(super) const DEVICE_VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";

/// Redirect identity bound into the final device authorization-code exchange.
pub(super) const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";

/// Token endpoint for code exchange and proactive refresh.
pub(super) const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

/// Revocation endpoint used on logout.
pub(super) const REVOKE_URL: &str = "https://auth.openai.com/oauth/revoke";

/// OAuth scopes requested during login.
pub(super) const OAUTH_SCOPES: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
