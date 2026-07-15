//! Duplicate-safe JSON decoding for policy-controlled documents.

use std::collections::BTreeMap;

use serde::de::{DeserializeOwned, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::{Number, Value};
use thiserror::Error;

/// Decode a complete JSON document without accepting duplicate object keys.
///
/// The ordinary `serde_json::Value` decoder keeps the last duplicate member.
/// Policy inputs instead reject ambiguity before deserializing their closed
/// schema.
///
/// # Errors
///
/// Returns a syntax error, a duplicate-key error, or a closed-schema decoding
/// error. Trailing non-whitespace bytes are rejected.
pub fn decode_strict_json<T>(bytes: &[u8]) -> Result<T, StrictJsonError>
where
    T: DeserializeOwned,
{
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = StrictValue::deserialize(&mut deserializer)
        .map_err(|source| StrictJsonError::Document { source })?;
    deserializer
        .end()
        .map_err(|source| StrictJsonError::Document { source })?;
    serde_json::from_value(value.0).map_err(|source| StrictJsonError::Schema { source })
}

/// Strict JSON decoding failures.
#[derive(Debug, Error)]
pub enum StrictJsonError {
    /// JSON syntax, duplicate members, or trailing bytes made the document
    /// ambiguous or malformed.
    #[error("invalid strict JSON document")]
    Document {
        /// Underlying syntax or visitor failure.
        #[source]
        source: serde_json::Error,
    },
    /// The unambiguous JSON value did not match the target closed schema.
    #[error("JSON document does not match its closed schema")]
    Schema {
        /// Underlying typed-deserialization failure.
        #[source]
        source: serde_json::Error,
    },
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictValueVisitor)
    }
}

struct StrictValueVisitor;

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = StrictValue;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value with unique object member names")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let reason = if value.is_finite() {
            "finite floating-point JSON numbers are not admitted"
        } else {
            "non-finite floating-point JSON numbers are not admitted"
        };
        Err(E::custom(reason))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.visit_string(value.to_owned())
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictValue(Value::Null))
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        StrictValue::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<StrictValue>()? {
            values.push(value.0);
        }
        Ok(StrictValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = BTreeMap::new();
        while let Some((key, value)) = map.next_entry::<String, StrictValue>()? {
            if values.insert(key, value.0).is_some() {
                return Err(serde::de::Error::custom("duplicate JSON object member"));
            }
        }
        Ok(StrictValue(Value::Object(values.into_iter().collect())))
    }
}
