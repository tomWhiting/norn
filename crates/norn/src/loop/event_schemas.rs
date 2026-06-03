//! Per-event type schemas and validation.
//!
//! Provides an [`EventType`] enum covering the event categories that can
//! have per-event output schemas, and an [`EventSchemaSet`] that maps each
//! type to an optional JSON Schema. Profiles configure which schemas are
//! active by populating the set.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Discriminant for the kinds of events that can carry per-event schemas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventType {
    /// Schema constraint for the model's text output.
    Text,
}

/// Result of validating event content against a per-event schema.
#[derive(Clone, Debug)]
pub struct EventValidationResult {
    /// The event type that was validated.
    pub event_type: EventType,
    /// Whether the content passed validation.
    pub passed: bool,
    /// Validation error messages (empty when `passed` is `true`).
    pub errors: Vec<String>,
}

/// A set of JSON Schemas keyed by [`EventType`].
///
/// When a schema is configured for a given event type, content of that type
/// will be validated against the schema. Unconfigured event types pass
/// validation unconditionally.
#[derive(Clone, Debug)]
pub struct EventSchemaSet {
    schemas: HashMap<EventType, serde_json::Value>,
}

impl EventSchemaSet {
    /// Create an empty schema set with no schemas configured.
    #[must_use]
    pub fn new() -> Self {
        Self {
            schemas: HashMap::new(),
        }
    }

    /// Register a JSON Schema for the given event type.
    pub fn set(&mut self, event_type: EventType, schema: serde_json::Value) {
        self.schemas.insert(event_type, schema);
    }

    /// Retrieve the schema for the given event type, if configured.
    #[must_use]
    pub fn get(&self, event_type: EventType) -> Option<&serde_json::Value> {
        self.schemas.get(&event_type)
    }

    /// Check whether a schema is configured for the given event type.
    #[must_use]
    pub fn has(&self, event_type: EventType) -> bool {
        self.schemas.contains_key(&event_type)
    }

    /// Iterate over the event types that have schemas configured.
    pub fn event_types(&self) -> impl Iterator<Item = &EventType> {
        self.schemas.keys()
    }

    /// Validate `content` against the schema for `event_type`.
    ///
    /// If no schema is configured for the event type, validation passes
    /// unconditionally with an empty error list.
    #[must_use]
    pub fn validate(
        &self,
        event_type: EventType,
        content: &serde_json::Value,
    ) -> EventValidationResult {
        let Some(schema) = self.schemas.get(&event_type) else {
            return EventValidationResult {
                event_type,
                passed: true,
                errors: Vec::new(),
            };
        };

        let validator = match jsonschema::validator_for(schema) {
            Ok(v) => v,
            Err(e) => {
                return EventValidationResult {
                    event_type,
                    passed: false,
                    errors: vec![format!("invalid schema: {e}")],
                };
            }
        };

        let errors: Vec<String> = validator
            .iter_errors(content)
            .map(|e| format!("{e}"))
            .collect();

        EventValidationResult {
            event_type,
            passed: errors.is_empty(),
            errors,
        }
    }
}

impl Default for EventSchemaSet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;

    fn text_schema() -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string" }
            },
            "required": ["text"]
        })
    }

    #[test]
    fn empty_set_has_no_schemas() {
        let set = EventSchemaSet::new();
        assert!(!set.has(EventType::Text));
        assert!(set.get(EventType::Text).is_none());
        assert_eq!(set.event_types().count(), 0);
    }

    #[test]
    fn set_and_get_schema() {
        let mut set = EventSchemaSet::new();
        let schema = text_schema();
        set.set(EventType::Text, schema.clone());
        assert!(set.has(EventType::Text));
        assert_eq!(set.get(EventType::Text), Some(&schema));
    }

    #[test]
    fn event_types_iterator() {
        let mut set = EventSchemaSet::new();
        set.set(EventType::Text, text_schema());
        let types: Vec<&EventType> = set.event_types().collect();
        assert_eq!(types.len(), 1);
        assert!(types.contains(&&EventType::Text));
    }

    #[test]
    fn validate_matching_content_passes() {
        let mut set = EventSchemaSet::new();
        set.set(EventType::Text, text_schema());
        let content = serde_json::json!({"text": "hello"});
        let result = set.validate(EventType::Text, &content);
        assert!(result.passed);
        assert!(result.errors.is_empty());
        assert_eq!(result.event_type, EventType::Text);
    }

    #[test]
    fn validate_non_matching_content_fails() {
        let mut set = EventSchemaSet::new();
        set.set(EventType::Text, text_schema());
        let content = serde_json::json!({"text": 42});
        let result = set.validate(EventType::Text, &content);
        assert!(!result.passed);
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn validate_missing_required_field_fails() {
        let mut set = EventSchemaSet::new();
        set.set(EventType::Text, text_schema());
        let content = serde_json::json!({"other": "value"});
        let result = set.validate(EventType::Text, &content);
        assert!(!result.passed);
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn validate_unconfigured_event_type_passes() {
        let set = EventSchemaSet::new();
        let content = serde_json::json!({"anything": "goes"});
        let result = set.validate(EventType::Text, &content);
        assert!(result.passed);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn event_type_serde_roundtrip() {
        let original = EventType::Text;
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: EventType = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, parsed);
    }

    #[test]
    fn event_type_copy() {
        let a = EventType::Text;
        let b = a;
        assert_eq!(a, b);
    }
}
