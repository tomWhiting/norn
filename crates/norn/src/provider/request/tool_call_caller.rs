//! Presence-preserving opaque `caller` metadata for Responses tool calls.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// Exact provider `caller` field retained for the matching tool output.
///
/// The Responses API distinguishes an absent field from explicit JSON `null`.
/// A present object is intentionally opaque so newer provider fields survive
/// persistence and replay unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ToolCallCaller {
    /// The originating call omitted `caller`.
    #[default]
    Absent,
    /// The exact present JSON value, including explicit `null`.
    Present(Value),
}

impl ToolCallCaller {
    /// Capture the field from an authoritative output item without normalizing
    /// its contents.
    #[must_use]
    pub fn from_item(raw: &Value) -> Self {
        raw.get("caller")
            .cloned()
            .map_or(Self::Absent, Self::Present)
    }

    /// Whether the provider omitted the field entirely.
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        matches!(self, Self::Absent)
    }

    /// Return the exact present JSON value.
    #[must_use]
    pub const fn value(&self) -> Option<&Value> {
        match self {
            Self::Absent => None,
            Self::Present(value) => Some(value),
        }
    }

    /// Whether a present value matches the public Responses caller union.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Absent | Self::Present(Value::Null) => true,
            Self::Present(Value::Object(caller)) => {
                match caller.get("type").and_then(Value::as_str) {
                    Some("direct") => true,
                    Some("program") => caller.get("caller_id").and_then(Value::as_str).is_some(),
                    _ => false,
                }
            }
            Self::Present(_) => false,
        }
    }
}

impl Serialize for ToolCallCaller {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Absent => serializer.serialize_unit(),
            Self::Present(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for ToolCallCaller {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Value::deserialize(deserializer).map(Self::Present)
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use serde_json::{Value, json};

    use super::ToolCallCaller;

    #[derive(Debug, Deserialize, Serialize)]
    struct CallerEnvelope {
        #[serde(default, skip_serializing_if = "ToolCallCaller::is_absent")]
        caller: ToolCallCaller,
    }

    #[test]
    fn serde_preserves_absent_null_and_object_callers() -> Result<(), serde_json::Error> {
        let absent = CallerEnvelope {
            caller: ToolCallCaller::Absent,
        };
        assert_eq!(serde_json::to_value(&absent)?, json!({}));

        let explicit_null: CallerEnvelope = serde_json::from_value(json!({"caller": null}))?;
        assert_eq!(explicit_null.caller, ToolCallCaller::Present(Value::Null));
        assert_eq!(
            serde_json::to_value(&explicit_null)?,
            json!({"caller": null})
        );

        let object = json!({"type": "program", "caller_id": "program_fixture"});
        let present: CallerEnvelope = serde_json::from_value(json!({"caller": &object}))?;
        assert_eq!(present.caller.value(), Some(&object));
        assert_eq!(serde_json::to_value(&present)?, json!({"caller": object}));
        Ok(())
    }
}
