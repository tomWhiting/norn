//! Tool catalog types and schema-driven catalog derivation.
//!
//! [`ToolCatalogEntry`] and [`ToolFieldHint`] describe tools and composite
//! subcommands for the `tool_search` catalog. Entries are *derived* from a
//! tool's JSON Schema — the same schema the `ToolArgs` derive builds from
//! the args struct's doc comments, types, and `#[tool_args(...)]`
//! attributes — so descriptions, required-ness, type hints, and enum
//! values have a single source of truth and never need a hand-maintained
//! hint table.
//!
//! [`Tool::catalog_entries`](super::traits::Tool::catalog_entries) uses
//! [`ToolCatalogEntry::from_tool_schema`] by default;
//! [`CompositeTool`](super::composite::CompositeTool) implementations get
//! per-command entries via [`composite_commands`].

use std::sync::Arc;

use serde_json::Value;

/// Field-level hints for constructing a cataloged subcommand call.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct ToolFieldHint {
    /// JSON field name accepted by the subcommand.
    pub name: String,
    /// Coarse field type hint such as `string`, `integer`, `boolean`, or `array`.
    pub type_hint: String,
    /// Whether the field must be supplied for this command.
    pub required: bool,
    /// Human-facing field description.
    pub description: String,
    /// Allowed values when the field is constrained to an enum-like set.
    pub enum_values: Vec<String>,
}

impl ToolFieldHint {
    /// Derive field hints from a JSON Schema object schema
    /// (`{"type": "object", "properties": {...}, "required": [...]}`).
    ///
    /// One hint per property, in the schema's property order (the
    /// workspace enables `serde_json`'s `preserve_order`, so this is the
    /// declaration order of the originating args struct). The `type_hint`
    /// is the property's `type`; a property constrained by `enum` without
    /// an explicit `type` is hinted as `string`; a property with no type
    /// information at all (e.g. a `serde_json::Value` field) is hinted as
    /// `any`. Schemas that are not object schemas yield no hints.
    #[must_use]
    pub fn from_object_schema(schema: &Value) -> Vec<Self> {
        let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
            return Vec::new();
        };
        let required: Vec<&str> = schema
            .get("required")
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();

        properties
            .iter()
            .map(|(name, prop)| {
                let enum_values: Vec<String> = prop
                    .get("enum")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let type_hint = prop.get("type").and_then(Value::as_str).map_or_else(
                    || {
                        if enum_values.is_empty() {
                            "any".to_string()
                        } else {
                            "string".to_string()
                        }
                    },
                    str::to_string,
                );
                Self {
                    name: name.clone(),
                    type_hint,
                    required: required.contains(&name.as_str()),
                    description: prop
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    enum_values,
                }
            })
            .collect()
    }
}

/// A single searchable tool or subcommand description.
#[derive(Clone, Debug)]
pub struct ToolCatalogEntry {
    /// Tool or subcommand name.
    pub name: String,
    /// Human-facing description.
    pub description: String,
    /// Parent composite tool name when this entry describes a subcommand.
    pub parent_tool: Option<String>,
    /// Concrete command value to pass to the parent composite tool.
    pub command_value: Option<String>,
    /// Field-level hints for constructing a call to this entry.
    pub fields: Vec<ToolFieldHint>,
}

impl ToolCatalogEntry {
    /// Construct a top-level tool catalog entry with no field hints.
    #[must_use]
    pub fn tool(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parent_tool: None,
            command_value: None,
            fields: Vec::new(),
        }
    }

    /// Construct a top-level tool entry with field hints derived from the
    /// tool's input schema via [`ToolFieldHint::from_object_schema`].
    #[must_use]
    pub fn from_tool_schema(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: &Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parent_tool: None,
            command_value: None,
            fields: ToolFieldHint::from_object_schema(input_schema),
        }
    }

    /// Construct a subcommand entry for a composite parent tool.
    #[must_use]
    pub fn subcommand(
        parent_tool: impl Into<String>,
        command_value: impl Into<String>,
        description: impl Into<String>,
        fields: Vec<ToolFieldHint>,
    ) -> Self {
        let parent_tool = parent_tool.into();
        let command_value = command_value.into();
        Self {
            name: command_value.clone(),
            description: description.into(),
            parent_tool: Some(parent_tool),
            command_value: Some(command_value),
            fields,
        }
    }

    /// Construct a parameterless subcommand entry for a composite parent tool.
    #[must_use]
    pub fn subcommand_no_fields(
        parent_tool: impl Into<String>,
        command_value: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self::subcommand(parent_tool, command_value, description, Vec::new())
    }

    /// The text BM25 indexes for this entry.
    #[must_use]
    pub fn searchable_text(&self) -> String {
        let parent = self.parent_tool.as_deref().unwrap_or_default();
        let command = self.command_value.as_deref().unwrap_or_default();
        format!("{} {parent} {command} {}", self.name, self.description)
    }
}

/// One subcommand extracted from an internally-tagged composite schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSchema {
    /// The command's wire value (the tag field's `const`).
    pub value: String,
    /// The command's description (the variant doc comment).
    pub description: String,
    /// Field hints for the command's own fields (tag field excluded).
    pub fields: Vec<ToolFieldHint>,
}

/// Extract the subcommands of an internally-tagged composite schema.
///
/// Expects the shape the `ToolArgs` derive emits for
/// `#[serde(tag = "<command_field>")]` enums: a top-level `oneOf` whose
/// variants are object schemas carrying the tag property as a `const`
/// alongside the variant's own fields. Variants that do not carry a
/// string `const` for `command_field` are skipped — they are not
/// addressable commands. A schema without `oneOf` yields no commands.
#[must_use]
pub fn composite_commands(schema: &Value, command_field: &str) -> Vec<CommandSchema> {
    let Some(variants) = schema.get("oneOf").and_then(Value::as_array) else {
        return Vec::new();
    };
    variants
        .iter()
        .filter_map(|variant| {
            let value = variant
                .get("properties")?
                .get(command_field)?
                .get("const")?
                .as_str()?
                .to_string();
            let fields = ToolFieldHint::from_object_schema(variant)
                .into_iter()
                .filter(|hint| hint.name != command_field)
                .collect();
            Some(CommandSchema {
                value,
                description: variant
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                fields,
            })
        })
        .collect()
}

/// Additional catalog entries supplied by an embedding runtime before the
/// final [`SharedToolCatalog`] snapshot is published.
pub struct ToolCatalogExtras(pub Vec<ToolCatalogEntry>);

/// Shared catalog handle.
///
/// Wrapping `Vec<ToolCatalogEntry>` in a named type lets the extension
/// map key on this type rather than the bare `Vec`, which keeps multiple
/// catalogues from accidentally clashing.
pub struct SharedToolCatalog(pub Arc<Vec<ToolCatalogEntry>>);

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use serde_json::json;

    use super::*;

    fn object_schema() -> Value {
        json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Keywords to search for."
                },
                "max_results": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Maximum number of results."
                },
                "mode": {
                    "type": "string",
                    "enum": ["fast", "thorough"],
                    "description": "Search mode."
                },
                "filters": {
                    "description": "Free-form filter object."
                }
            },
            "additionalProperties": false
        })
    }

    #[test]
    fn field_hints_derive_name_type_required_description_and_enums() {
        let hints = ToolFieldHint::from_object_schema(&object_schema());
        assert_eq!(hints.len(), 4);

        assert_eq!(
            hints[0],
            ToolFieldHint {
                name: "query".to_string(),
                type_hint: "string".to_string(),
                required: true,
                description: "Keywords to search for.".to_string(),
                enum_values: Vec::new(),
            }
        );
        assert_eq!(hints[1].name, "max_results");
        assert_eq!(hints[1].type_hint, "integer");
        assert!(!hints[1].required);
        assert_eq!(hints[2].enum_values, vec!["fast", "thorough"]);
        assert_eq!(
            hints[3].type_hint, "any",
            "a property with no type info is hinted as `any`",
        );
    }

    #[test]
    fn field_hints_from_non_object_schema_are_empty() {
        assert!(ToolFieldHint::from_object_schema(&json!({"type": "string"})).is_empty());
        assert!(ToolFieldHint::from_object_schema(&json!(null)).is_empty());
    }

    #[test]
    fn enum_without_type_is_hinted_as_string() {
        let schema = json!({
            "type": "object",
            "properties": { "level": { "enum": ["low", "high"] } },
            "required": []
        });
        let hints = ToolFieldHint::from_object_schema(&schema);
        assert_eq!(hints[0].type_hint, "string");
        assert_eq!(hints[0].enum_values, vec!["low", "high"]);
    }

    #[test]
    fn from_tool_schema_builds_entry_with_derived_fields() {
        let entry =
            ToolCatalogEntry::from_tool_schema("search", "Search file contents", &object_schema());
        assert_eq!(entry.name, "search");
        assert_eq!(entry.description, "Search file contents");
        assert!(entry.parent_tool.is_none());
        assert!(entry.command_value.is_none());
        assert_eq!(entry.fields.len(), 4);
        assert_eq!(entry.fields[0].name, "query");
    }

    fn tagged_composite_schema() -> Value {
        json!({
            "oneOf": [
                {
                    "type": "object",
                    "description": "Create a new task.",
                    "properties": {
                        "action": { "const": "create" },
                        "description": {
                            "type": "string",
                            "description": "Task description."
                        }
                    },
                    "required": ["action", "description"],
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "description": "List tasks.",
                    "properties": {
                        "action": { "const": "list" },
                        "status": {
                            "type": "string",
                            "enum": ["pending", "completed"],
                            "description": "Filter by status."
                        }
                    },
                    "required": ["action"],
                    "additionalProperties": false
                }
            ]
        })
    }

    #[test]
    fn composite_commands_extracts_values_descriptions_and_fields() {
        let commands = composite_commands(&tagged_composite_schema(), "action");
        assert_eq!(commands.len(), 2);

        assert_eq!(commands[0].value, "create");
        assert_eq!(commands[0].description, "Create a new task.");
        assert_eq!(commands[0].fields.len(), 1, "tag field is excluded");
        assert_eq!(commands[0].fields[0].name, "description");
        assert!(commands[0].fields[0].required);

        assert_eq!(commands[1].value, "list");
        assert!(!commands[1].fields[0].required);
        assert_eq!(
            commands[1].fields[0].enum_values,
            vec!["pending", "completed"]
        );
    }

    #[test]
    fn composite_commands_on_non_composite_schema_is_empty() {
        assert!(composite_commands(&object_schema(), "action").is_empty());
        assert!(composite_commands(&json!({"oneOf": "nope"}), "action").is_empty());
    }

    #[test]
    fn composite_commands_skips_variants_without_string_const_tag() {
        let schema = json!({
            "oneOf": [
                { "type": "object", "properties": { "action": { "const": 3 } } },
                {
                    "type": "object",
                    "description": "Valid.",
                    "properties": { "action": { "const": "ok" } },
                    "required": ["action"]
                }
            ]
        });
        let commands = composite_commands(&schema, "action");
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].value, "ok");
    }
}
