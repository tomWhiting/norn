//! Versioned SHA-256 digests and canonical JSON encoding.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

const DIGEST_LEN: usize = 32;
const HEX_LEN: usize = DIGEST_LEN * 2;
const HEX: &[u8; 16] = b"0123456789abcdef";

/// A complete SHA-256 digest rendered as 64 lowercase hexadecimal digits.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Digest([u8; DIGEST_LEN]);

impl Digest {
    /// Construct a digest from its complete bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; DIGEST_LEN]) -> Self {
        Self(bytes)
    }

    /// Borrow the complete digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; DIGEST_LEN] {
        &self.0
    }

    /// Render the complete digest using lowercase hexadecimal digits.
    #[must_use]
    pub fn to_hex(self) -> String {
        let mut encoded = String::with_capacity(HEX_LEN);
        for byte in self.0 {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        encoded
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Digest")
            .field(&self.to_hex())
            .finish()
    }
}

impl FromStr for Digest {
    type Err = DigestParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != HEX_LEN {
            return Err(DigestParseError::Length {
                actual: value.len(),
            });
        }
        let mut bytes = [0_u8; DIGEST_LEN];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let high =
                decode_hex(pair[0]).ok_or(DigestParseError::InvalidHex { index: index * 2 })?;
            let low = decode_hex(pair[1]).ok_or(DigestParseError::InvalidHex {
                index: index * 2 + 1,
            })?;
            bytes[index] = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Hash arbitrary bytes without applying any text normalization.
#[must_use]
pub fn digest_bytes(bytes: &[u8]) -> Digest {
    Digest(Sha256::digest(bytes).into())
}

/// Encode a JSON value canonically and hash the exact encoded bytes.
///
/// Object keys are ordered lexicographically, arrays retain their order, and
/// insignificant whitespace is omitted. Floating-point numbers are rejected;
/// only exact signed or unsigned JSON integers are admitted.
///
/// # Errors
///
/// Returns [`CanonicalJsonError::FloatingPointNumber`] for an inexact number,
/// or [`CanonicalJsonError::Encoding`] if JSON string encoding fails.
pub fn digest_json(value: &Value) -> Result<Digest, CanonicalJsonError> {
    canonical_json_bytes(value).map(|bytes| digest_bytes(&bytes))
}

/// Encode one JSON value using the deterministic representation hashed by
/// [`digest_json`].
///
/// # Errors
///
/// Returns an error for floating-point numbers or failed string encoding.
pub fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>, CanonicalJsonError> {
    let mut encoded = Vec::new();
    encode_value(value, &mut encoded)?;
    Ok(encoded)
}

/// Failures parsing a complete hexadecimal digest.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DigestParseError {
    /// The input was not exactly 64 bytes long.
    #[error("SHA-256 digest has length {actual}; expected 64")]
    Length {
        /// Observed byte length.
        actual: usize,
    },
    /// A byte was not a lowercase hexadecimal digit.
    #[error("SHA-256 digest contains invalid lowercase hexadecimal at byte {index}")]
    InvalidHex {
        /// Zero-based byte position of the invalid digit.
        index: usize,
    },
}

/// Unsupported or failed canonical JSON encoding.
#[derive(Debug, Error)]
pub enum CanonicalJsonError {
    /// Canonical policy documents admit integers but not floating point.
    #[error("canonical JSON does not support floating-point numbers")]
    FloatingPointNumber,
    /// `serde_json` could not encode a JSON string.
    #[error("canonical JSON string encoding failed")]
    Encoding(#[source] serde_json::Error),
}

fn encode_value(value: &Value, output: &mut Vec<u8>) -> Result<(), CanonicalJsonError> {
    match value {
        Value::Null => output.extend_from_slice(b"null"),
        Value::Bool(true) => output.extend_from_slice(b"true"),
        Value::Bool(false) => output.extend_from_slice(b"false"),
        Value::Number(number) => {
            if number.as_i64().is_none() && number.as_u64().is_none() {
                return Err(CanonicalJsonError::FloatingPointNumber);
            }
            output.extend_from_slice(number.to_string().as_bytes());
        }
        Value::String(string) => encode_string(string, output)?,
        Value::Array(values) => {
            output.push(b'[');
            for (index, member) in values.iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                encode_value(member, output)?;
            }
            output.push(b']');
        }
        Value::Object(object) => {
            output.push(b'{');
            let mut members: Vec<(&String, &Value)> = object.iter().collect();
            members.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            for (index, (key, member)) in members.into_iter().enumerate() {
                if index != 0 {
                    output.push(b',');
                }
                encode_string(key, output)?;
                output.push(b':');
                encode_value(member, output)?;
            }
            output.push(b'}');
        }
    }
    Ok(())
}

fn encode_string(value: &str, output: &mut Vec<u8>) -> Result<(), CanonicalJsonError> {
    serde_json::to_writer(output, value).map_err(CanonicalJsonError::Encoding)
}

const fn decode_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
