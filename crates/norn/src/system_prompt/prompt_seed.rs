//! Stable commitment to provider-visible non-System prompt authority.

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::authority::PromptAuthority;
use super::plan::PromptPlan;

const PROMPT_SEED_DOMAIN: &[u8] = b"norn.prompt-seed.v1\0";
const OPERATOR_RUNTIME_DOMAIN: &[u8] = b"norn.prompt-seed.operator-runtime.v1\0";
const SHA256_BYTES: usize = 32;
const SHA256_HEX_CHARS: usize = SHA256_BYTES * 2;

/// SHA-256 commitment to an ordered stable Developer/User prompt seed.
///
/// System fragments are intentionally excluded because Responses
/// `instructions` are request-local and may change without invalidating a
/// provider-side response anchor. Source, derived authority, and exact UTF-8
/// content are independently length-framed for every included fragment.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct PromptSeedFingerprint([u8; SHA256_BYTES]);

impl PromptSeedFingerprint {
    /// Commit to the ordered non-System fragments in `plan`.
    #[must_use]
    pub fn from_plan(plan: &PromptPlan) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(PROMPT_SEED_DOMAIN);
        let included_count = plan
            .fragments()
            .iter()
            .filter(|fragment| fragment.authority() != PromptAuthority::System)
            .count();
        hash_count(&mut hasher, included_count);
        for fragment in plan
            .fragments()
            .iter()
            .filter(|fragment| fragment.authority() != PromptAuthority::System)
        {
            hash_framed(&mut hasher, fragment.source().as_str().as_bytes());
            hash_framed(&mut hasher, fragment.authority().as_str().as_bytes());
            hash_framed(&mut hasher, fragment.content().as_bytes());
        }
        Self(hasher.finalize().into())
    }

    /// Commitment for a legacy prompt with no typed non-System seed.
    #[must_use]
    pub fn empty() -> Self {
        Self::from_plan(&PromptPlan::new())
    }

    /// Bind current volatile operator command output at Developer authority.
    ///
    /// Provider-threaded Responses can reuse an anchor while this exact
    /// content remains stable. A changed or removed value produces a distinct
    /// seed and therefore forces replay instead of promoting it into System
    /// instructions or accumulating stale Developer rows in the remote thread.
    #[must_use]
    pub fn with_operator_runtime_context(self, content: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(OPERATOR_RUNTIME_DOMAIN);
        hash_framed(&mut hasher, &self.0);
        hash_framed(&mut hasher, b"operator_prompt_command");
        hash_framed(&mut hasher, PromptAuthority::Developer.as_str().as_bytes());
        hash_framed(&mut hasher, content.as_bytes());
        Self(hasher.finalize().into())
    }

    fn encode_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut encoded = String::with_capacity(SHA256_HEX_CHARS);
        for byte in self.0 {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        encoded
    }

    fn decode_hex(value: &str) -> Option<Self> {
        if value.len() != SHA256_HEX_CHARS {
            return None;
        }
        let mut digest = [0_u8; SHA256_BYTES];
        for (slot, pair) in digest.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
            let high = decode_hex_nibble(pair[0])?;
            let low = decode_hex_nibble(pair[1])?;
            *slot = (high << 4) | low;
        }
        Some(Self(digest))
    }
}

impl fmt::Debug for PromptSeedFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PromptSeedFingerprint([REDACTED])")
    }
}

impl Serialize for PromptSeedFingerprint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.encode_hex())
    }
}

impl<'de> Deserialize<'de> for PromptSeedFingerprint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        Self::decode_hex(&encoded).ok_or_else(|| {
            serde::de::Error::custom(
                "prompt seed fingerprint must be 64 lowercase hexadecimal digits",
            )
        })
    }
}

fn hash_framed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(bytes.len().to_string().as_bytes());
    hasher.update(b":");
    hasher.update(bytes);
}

fn hash_count(hasher: &mut Sha256, count: usize) {
    hasher.update(count.to_string().as_bytes());
    hasher.update(b"\0");
}

const fn decode_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
