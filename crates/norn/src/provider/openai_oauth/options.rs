//! Network and credential-coordination timing for the OAuth stack.

use std::time::Duration;

use super::credential_lock_timing::{CredentialLockTiming, CredentialLockTimingError};

/// Timing options applied across OAuth network exchanges and credential
/// coordination.
///
/// Threaded explicitly through [`AuthManager`], [`ServerOptions`], and
/// [`logout_with_revoke`] so embedders control every network and lock-wait
/// deadline in the credential lifecycle. The [`Default`] preserves the
/// existing network deadlines and applies the owner-approved credential-lock
/// timing policy.
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
    /// Total wait for a device-code authorization to be approved. The
    /// authority's current device codes expire after 15 minutes; callers may
    /// set a shorter policy or track a future authority contract explicitly.
    pub device_code_timeout: Duration,
    /// Maximum time to wait for another Norn process to finish a credential
    /// transaction. This does not coordinate lock-ignoring foreign writers;
    /// raw revision checks detect observed foreign changes instead.
    pub credential_lock_timeout: Duration,
    /// Delay between inter-process lock probes while another process owns the
    /// credential transaction. Process-local waiters use notifications.
    pub credential_lock_poll_interval: Duration,
}

impl OAuthHttpOptions {
    /// Documented, owner-approved default (pre-existing hardcoded value)
    /// for [`OAuthHttpOptions::request_timeout`]: 10 seconds.
    pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
    /// Documented, owner-approved default (pre-existing hardcoded value)
    /// for [`OAuthHttpOptions::callback_timeout`]: 5 minutes.
    pub const DEFAULT_CALLBACK_TIMEOUT: Duration = Duration::from_mins(5);
    /// Current `OpenAI` device-code lifetime, as implemented by Codex: 15 minutes.
    pub const DEFAULT_DEVICE_CODE_TIMEOUT: Duration = Duration::from_mins(15);
    /// Owner-approved bounded wait for one credential transaction: 30 seconds.
    pub const DEFAULT_CREDENTIAL_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
    /// Owner-approved inter-process lock polling cadence: 25 milliseconds.
    pub const DEFAULT_CREDENTIAL_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(25);

    pub(crate) fn credential_lock_timing(
        self,
    ) -> Result<CredentialLockTiming, CredentialLockTimingError> {
        CredentialLockTiming::new(
            self.credential_lock_timeout,
            self.credential_lock_poll_interval,
        )
    }
}

impl Default for OAuthHttpOptions {
    fn default() -> Self {
        Self {
            request_timeout: Self::DEFAULT_REQUEST_TIMEOUT,
            callback_timeout: Self::DEFAULT_CALLBACK_TIMEOUT,
            device_code_timeout: Self::DEFAULT_DEVICE_CODE_TIMEOUT,
            credential_lock_timeout: Self::DEFAULT_CREDENTIAL_LOCK_TIMEOUT,
            credential_lock_poll_interval: Self::DEFAULT_CREDENTIAL_LOCK_POLL_INTERVAL,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_use_owner_approved_credential_lock_timing() {
        let options = OAuthHttpOptions::default();

        assert_eq!(options.credential_lock_timeout, Duration::from_secs(30));
        assert_eq!(options.device_code_timeout, Duration::from_mins(15));
        assert_eq!(
            options.credential_lock_poll_interval,
            Duration::from_millis(25)
        );
        assert!(options.credential_lock_timing().is_ok());
    }

    #[test]
    fn credential_lock_timing_preserves_programmatic_overrides()
    -> Result<(), CredentialLockTimingError> {
        let options = OAuthHttpOptions {
            credential_lock_timeout: Duration::from_secs(7),
            credential_lock_poll_interval: Duration::from_millis(13),
            ..OAuthHttpOptions::default()
        };

        let timing = options.credential_lock_timing()?;
        assert_eq!(timing.deadline(), Duration::from_secs(7));
        assert_eq!(timing.poll_interval(), Duration::from_millis(13));
        Ok(())
    }
}
