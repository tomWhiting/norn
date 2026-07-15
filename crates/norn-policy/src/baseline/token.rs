//! Closed machine tokens admitted by legacy governance.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// A bounded machine identifier that cannot contain prose.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GovernanceToken(String);

impl GovernanceToken {
    /// Parse one lowercase machine token.
    ///
    /// # Errors
    ///
    /// Returns the exact structural reason the token is not admitted.
    pub fn parse(value: impl Into<String>) -> Result<Self, GovernanceTokenError> {
        let value = value.into();
        if value.is_empty() {
            return Err(GovernanceTokenError::Empty);
        }
        if value.len() > 128 {
            return Err(GovernanceTokenError::TooLong);
        }
        if !value.bytes().all(is_token_byte) {
            return Err(GovernanceTokenError::UnsupportedByte);
        }
        Ok(Self(value))
    }

    /// Borrow the validated token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for GovernanceToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("GovernanceToken")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for GovernanceToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for GovernanceToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for GovernanceToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

/// Invalid governance-token structure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum GovernanceTokenError {
    /// No token bytes were supplied.
    #[error("governance token is empty")]
    Empty,
    /// The fixed machine-token bound was exceeded.
    #[error("governance token exceeds 128 bytes")]
    TooLong,
    /// A byte was outside the closed token grammar.
    #[error("governance token contains an unsupported byte")]
    UnsupportedByte,
}

const fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b':' | b'-')
}
