//! Non-disclosing identity keys for named OAuth account deduplication.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::super::types::AuthDotJson;

const ACCOUNT_IDENTITY_DOMAIN: &[u8] = b"norn.named-account.identity.v1\0";

/// Stable private-catalog key for one validated remote account identity.
#[derive(Clone, Copy, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub(in crate::provider::openai_oauth) struct AccountIdentityFingerprint([u8; 32]);

impl AccountIdentityFingerprint {
    pub(super) fn from_auth(auth: &AuthDotJson) -> Option<Self> {
        let account_id = auth
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.account_id.as_deref())?;
        let mut digest = Sha256::new();
        digest.update(ACCOUNT_IDENTITY_DOMAIN);
        digest.update(account_id.as_bytes());
        Some(Self(digest.finalize().into()))
    }
}

impl std::fmt::Debug for AccountIdentityFingerprint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AccountIdentityFingerprint([REDACTED])")
    }
}

#[cfg(test)]
#[path = "account_identity_tests.rs"]
mod tests;
