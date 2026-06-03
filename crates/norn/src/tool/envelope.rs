//! Tool call envelope with model args, runtime inputs, and open metadata.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Wraps a tool call with its full execution context.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolEnvelope {
    /// Model-assigned identifier for this tool call.
    pub tool_call_id: String,
    /// Name of the tool being called.
    pub tool_name: String,
    /// Model-supplied parameters matching the tool's input schema.
    pub model_args: serde_json::Value,
    /// Runtime-supplied inputs accumulated since the last tool boundary.
    pub runtime_inputs: RuntimeInputs,
    /// Open, schemaless metadata field the model can populate.
    pub metadata: serde_json::Value,
}

/// Inputs accumulated by the runtime between tool boundaries.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeInputs {
    /// Messages received from users, other agents, or the orchestrator.
    pub inbound_messages: Vec<InboundMessage>,
    /// Diagnostic reports from background processes.
    pub diagnostics: Vec<DiagnosticReport>,
    /// Filesystem changes detected since the last tool boundary.
    pub filesystem_changes: Vec<FileChange>,
}

/// A message received during the agent loop.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InboundMessage {
    /// Who sent the message.
    pub author: String,
    /// Message content.
    pub content: String,
    /// When the message was sent.
    pub timestamp: DateTime<Utc>,
}

/// A diagnostic report from a background process.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiagnosticReport {
    /// Source of the diagnostic (e.g. "clippy", "cargo check").
    pub source: String,
    /// Severity level (e.g. "error", "warning").
    pub severity: String,
    /// Diagnostic message.
    pub message: String,
    /// File path the diagnostic applies to, if any.
    pub file_path: Option<String>,
}

/// A detected filesystem change.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileChange {
    /// Path of the changed file.
    pub path: String,
    /// What kind of change occurred.
    pub change_type: FileChangeType,
}

/// Type of filesystem change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileChangeType {
    /// A new file was created.
    Created,
    /// An existing file was modified.
    Modified,
    /// A file was deleted.
    Deleted,
}

/// JSON property name for the model-supplied description of its intent
/// when calling a tool. Prefixed to avoid collisions with tool parameters.
pub const ENVELOPE_DESCRIPTION_KEY: &str = "tool_use_description";

/// JSON property name for the model-supplied metadata attached to a tool
/// call. Prefixed to avoid collisions with tool parameters.
pub const ENVELOPE_METADATA_KEY: &str = "tool_use_metadata";

/// Result of splitting envelope fields from raw model arguments.
#[derive(Clone, Debug)]
pub struct EnvelopeSplit {
    /// The tool's own parameters with envelope fields removed.
    pub tool_args: serde_json::Value,
    /// Model-supplied description of intent (from `tool_use_description`).
    pub description: Option<String>,
    /// Model-supplied metadata (from `tool_use_metadata`).
    pub metadata: serde_json::Value,
}

/// Extract envelope fields (`tool_use_description`, `tool_use_metadata`)
/// from raw model arguments, returning the clean tool args separately.
///
/// When the input is not a JSON object or the envelope fields are absent,
/// the original value passes through unchanged.
pub fn split_envelope_fields(mut raw: serde_json::Value) -> EnvelopeSplit {
    let Some(map) = raw.as_object_mut() else {
        return EnvelopeSplit {
            tool_args: raw,
            description: None,
            metadata: serde_json::Value::Null,
        };
    };

    let description = map
        .remove(ENVELOPE_DESCRIPTION_KEY)
        .and_then(|v| v.as_str().map(str::to_owned));

    let metadata = map
        .remove(ENVELOPE_METADATA_KEY)
        .unwrap_or(serde_json::Value::Null);

    EnvelopeSplit {
        tool_args: raw,
        description,
        metadata,
    }
}

/// Inject the envelope field definitions into a tool's `input_schema`,
/// producing the schema the model actually sees.
///
/// The envelope fields are added as optional properties alongside the
/// tool's own parameters. `additionalProperties` is left as the tool
/// declared it.
pub fn wrap_schema_with_envelope(mut schema: serde_json::Value) -> serde_json::Value {
    let Some(props) = schema
        .as_object_mut()
        .and_then(|m| m.get_mut("properties"))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return schema;
    };

    props.insert(
        ENVELOPE_DESCRIPTION_KEY.to_owned(),
        serde_json::json!({
            "type": "string",
            "description": "Brief description of what you are doing with this tool call and why."
        }),
    );
    props.insert(
        ENVELOPE_METADATA_KEY.to_owned(),
        serde_json::json!({
            "type": "object",
            "description": "Optional tags, task references, or annotations for this tool call."
        }),
    );

    if let Some(required) = schema
        .as_object_mut()
        .and_then(|m| m.get_mut("required"))
        .and_then(serde_json::Value::as_array_mut)
    {
        let desc_key = serde_json::Value::String(ENVELOPE_DESCRIPTION_KEY.to_owned());
        if !required.contains(&desc_key) {
            required.push(desc_key);
        }
    }

    schema
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn split_extracts_envelope_fields() {
        let raw = json!({
            "tool_use_description": "reading config",
            "path": "/etc/config.toml",
            "tool_use_metadata": {"task": "T-1"}
        });
        let split = split_envelope_fields(raw);
        assert_eq!(split.description.as_deref(), Some("reading config"));
        assert_eq!(split.metadata, json!({"task": "T-1"}));
        assert_eq!(split.tool_args, json!({"path": "/etc/config.toml"}));
    }

    #[test]
    fn split_handles_missing_envelope_fields() {
        let raw = json!({"path": "/foo"});
        let split = split_envelope_fields(raw);
        assert!(split.description.is_none());
        assert_eq!(split.metadata, serde_json::Value::Null);
        assert_eq!(split.tool_args, json!({"path": "/foo"}));
    }

    #[test]
    fn split_handles_non_object() {
        let raw = json!("bare string");
        let split = split_envelope_fields(raw.clone());
        assert_eq!(split.tool_args, raw);
        assert!(split.description.is_none());
    }

    #[test]
    fn wrap_schema_injects_envelope_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"}
            },
            "required": ["path"]
        });
        let wrapped = wrap_schema_with_envelope(schema);
        let props = wrapped["properties"].as_object().unwrap();
        assert!(props.contains_key("tool_use_description"));
        assert!(props.contains_key("tool_use_metadata"));
        assert!(props.contains_key("path"));
        let required = wrapped["required"].as_array().expect("required array");
        assert!(
            required.contains(&json!("path")),
            "original required fields preserved",
        );
        assert!(
            required.contains(&json!("tool_use_description")),
            "tool_use_description added to required",
        );
    }

    #[test]
    fn wrap_schema_passthrough_non_object() {
        let schema = json!("not an object schema");
        let wrapped = wrap_schema_with_envelope(schema.clone());
        assert_eq!(wrapped, schema);
    }
}
