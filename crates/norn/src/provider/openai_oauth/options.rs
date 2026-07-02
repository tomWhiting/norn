//! HTTP options for the OAuth stack.

use std::time::Duration;

/// HTTP timeouts applied across the OAuth stack (token refresh, token
/// revocation, the browser-login authorization-code exchange, and the
/// local login-callback wait).
///
/// Threaded explicitly through [`AuthManager`], [`ServerOptions`], and
/// [`logout_with_revoke`] so embedders control every network deadline in
/// the credential lifecycle. The [`Default`] carries the documented,
/// owner-approved values that were previously hardcoded.
///
/// [`AuthManager`]: super::manager::AuthManager
/// [`ServerOptions`]: super::login_server::ServerOptions
/// [`logout_with_revoke`]: super::revoke::logout_with_revoke
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OAuthHttpOptions {
    /// Whole-request deadline for each HTTP exchange against the OAuth
    /// authority (refresh, revoke, and the authorization-code exchange).
    /// These are small JSON round-trips, so a whole-request deadline is
    /// correct here — unlike provider streaming, nothing is legitimately
    /// long-lived.
    pub request_timeout: Duration,
    /// Total wait for the browser to deliver the OAuth redirect to the
    /// local login-callback server before the login flow fails.
    pub callback_timeout: Duration,
}

impl OAuthHttpOptions {
    /// Documented, owner-approved default (pre-existing hardcoded value)
    /// for [`OAuthHttpOptions::request_timeout`]: 10 seconds.
    pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
    /// Documented, owner-approved default (pre-existing hardcoded value)
    /// for [`OAuthHttpOptions::callback_timeout`]: 5 minutes.
    pub const DEFAULT_CALLBACK_TIMEOUT: Duration = Duration::from_mins(5);
}

impl Default for OAuthHttpOptions {
    fn default() -> Self {
        Self {
            request_timeout: Self::DEFAULT_REQUEST_TIMEOUT,
            callback_timeout: Self::DEFAULT_CALLBACK_TIMEOUT,
        }
    }
}
