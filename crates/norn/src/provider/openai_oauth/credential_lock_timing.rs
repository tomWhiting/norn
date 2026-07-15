//! Validated timing policy for inter-process OAuth credential locks.

use std::time::Duration;

/// Positive timing values required by credential lock acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CredentialLockTiming {
    deadline: Duration,
    poll_interval: Duration,
}

/// Invalid credential lock timing supplied by an embedder.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum CredentialLockTimingError {
    /// A zero deadline would make every acquisition fail immediately.
    #[error("credential lock acquisition deadline must be greater than zero")]
    ZeroDeadline,
    /// A zero polling interval would busy-loop while another process holds the lock.
    #[error("credential lock polling interval must be greater than zero")]
    ZeroPollInterval,
}

impl CredentialLockTiming {
    pub(crate) fn new(
        deadline: Duration,
        poll_interval: Duration,
    ) -> Result<Self, CredentialLockTimingError> {
        if deadline.is_zero() {
            return Err(CredentialLockTimingError::ZeroDeadline);
        }
        if poll_interval.is_zero() {
            return Err(CredentialLockTimingError::ZeroPollInterval);
        }
        Ok(Self {
            deadline,
            poll_interval,
        })
    }

    pub(crate) fn deadline(self) -> Duration {
        self.deadline
    }

    pub(crate) fn poll_interval(self) -> Duration {
        self.poll_interval
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_deadline_is_rejected() {
        assert_eq!(
            CredentialLockTiming::new(Duration::ZERO, Duration::from_millis(25)),
            Err(CredentialLockTimingError::ZeroDeadline)
        );
    }

    #[test]
    fn zero_poll_interval_is_rejected() {
        assert_eq!(
            CredentialLockTiming::new(Duration::from_secs(30), Duration::ZERO),
            Err(CredentialLockTimingError::ZeroPollInterval)
        );
    }
}
