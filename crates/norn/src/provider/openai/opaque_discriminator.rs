//! Non-disclosing identifiers for provider-controlled terminal discriminators.

use std::sync::OnceLock;

use hmac::{Hmac, Mac};
use rand::TryRngCore;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

static PROCESS_KEY: OnceLock<Result<[u8; 32], OpaqueTagError>> = OnceLock::new();

/// Provider terminal field whose unknown value is being identified.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TerminalDiscriminator {
    /// `response.failed.response.error.code`.
    FailedCode,
    /// `response.incomplete.response.incomplete_details.reason`.
    IncompleteReason,
}

impl TerminalDiscriminator {
    fn domain(self) -> &'static [u8] {
        match self {
            Self::FailedCode => b"norn:responses:failed-code:v1",
            Self::IncompleteReason => b"norn:responses:incomplete-reason:v1",
        }
    }
}

/// Failure to initialize a non-disclosing terminal discriminator tag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OpaqueTagError {
    /// The operating system did not provide process-local random key material.
    EntropyUnavailable,
    /// The keyed MAC implementation rejected its fixed-size key.
    MacInitialization,
}

impl std::fmt::Display for OpaqueTagError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::EntropyUnavailable => "OS randomness unavailable",
            Self::MacInitialization => "keyed diagnostic initialization failed",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for OpaqueTagError {}

/// Returns a process-local opaque tag for an unknown provider discriminator.
pub(super) fn opaque_tag(
    kind: TerminalDiscriminator,
    raw_value: &str,
) -> Result<String, OpaqueTagError> {
    let key = PROCESS_KEY
        .get_or_init(generate_process_key)
        .as_ref()
        .map_err(|error| *error)?;
    compute_opaque_tag(key, kind, raw_value)
}

fn generate_process_key() -> Result<[u8; 32], OpaqueTagError> {
    generate_key_with(|key| rand::rngs::OsRng.try_fill_bytes(key))
}

fn generate_key_with<FillError>(
    fill: impl FnOnce(&mut [u8]) -> Result<(), FillError>,
) -> Result<[u8; 32], OpaqueTagError> {
    let mut key = [0_u8; 32];
    if fill(&mut key).is_err() {
        return Err(OpaqueTagError::EntropyUnavailable);
    }
    Ok(key)
}

fn compute_opaque_tag(
    key: &[u8; 32],
    kind: TerminalDiscriminator,
    raw_value: &str,
) -> Result<String, OpaqueTagError> {
    let Ok(mut mac) = HmacSha256::new_from_slice(key) else {
        return Err(OpaqueTagError::MacInitialization);
    };
    mac.update(kind.domain());
    mac.update(&[0]);
    mac.update(raw_value.as_bytes());
    Ok(hex_lower(&mac.finalize().into_bytes()))
}

#[cfg(test)]
fn opaque_tag_with_test_key(
    key: &[u8; 32],
    kind: TerminalDiscriminator,
    raw_value: &str,
) -> Result<String, OpaqueTagError> {
    compute_opaque_tag(key, kind, raw_value)
}

fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn deterministic_test_key_preserves_equality_without_raw_text() -> TestResult {
        let key = [0x5a; 32];
        let first = opaque_tag_with_test_key(
            &key,
            TerminalDiscriminator::FailedCode,
            "private-future-code",
        )?;
        let repeated = opaque_tag_with_test_key(
            &key,
            TerminalDiscriminator::FailedCode,
            "private-future-code",
        )?;
        let distinct = opaque_tag_with_test_key(
            &key,
            TerminalDiscriminator::FailedCode,
            "another-private-code",
        )?;

        assert_eq!(first, repeated);
        assert_ne!(first, distinct);
        assert_eq!(first.len(), 64);
        assert!(!first.contains("private"));
        Ok(())
    }

    #[test]
    fn terminal_categories_are_domain_separated() -> TestResult {
        let key = [0x42; 32];
        let failed = opaque_tag_with_test_key(
            &key,
            TerminalDiscriminator::FailedCode,
            "same-private-value",
        )?;
        let incomplete = opaque_tag_with_test_key(
            &key,
            TerminalDiscriminator::IncompleteReason,
            "same-private-value",
        )?;

        assert_ne!(failed, incomplete);
        Ok(())
    }

    #[test]
    fn entropy_failure_is_typed_and_has_no_fallback() {
        let result = generate_key_with::<()>(|_| Err(()));
        assert_eq!(result, Err(OpaqueTagError::EntropyUnavailable));
    }
}
