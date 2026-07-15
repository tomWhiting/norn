//! Raw credential serialization and non-disclosing revision identity.

use sha2::{Digest as _, Sha256};

use super::types::AuthDotJson;

/// Raw-byte identity of one observed `auth.json` version.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct CredentialRevision(pub(super) [u8; 32]);

impl std::fmt::Debug for CredentialRevision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialRevision([REDACTED])")
    }
}

/// Serialize a credential exactly as the transaction publishes it.
pub(crate) fn serialize_auth(
    auth: &AuthDotJson,
) -> Result<(Vec<u8>, CredentialRevision), serde_json::Error> {
    let mut raw = serde_json::to_vec_pretty(auth)?;
    raw.push(b'\n');
    let revision = revision(&raw);
    Ok((raw, revision))
}

/// Hash exact credential bytes without exposing them.
pub(crate) fn revision(raw: &[u8]) -> CredentialRevision {
    CredentialRevision(Sha256::digest(raw).into())
}
