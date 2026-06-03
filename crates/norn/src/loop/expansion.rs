//! Variable expansion helpers (N-023 R5).
//!
//! Wraps [`crate::integration::variables::expand`] so the agent loop can
//! substitute `{{var}}` placeholders in the system instruction and tool
//! descriptions before each provider call. Failures are advisory: when
//! expansion errors (unknown variable, shell timeout, etc.) the original
//! template is returned unchanged and the failure is logged at `warn`
//! level so observers can spot mis-configured stores.

use crate::integration::variables::{VariableStore, expand};
use crate::provider::request::ToolDefinition;

/// Expand `{{var}}` placeholders in `template` using `store`.
///
/// On any failure the original `template` is returned and a warning is
/// emitted. This matches the brief's "do not block the call" requirement.
pub async fn expand_string(template: &str, store: &VariableStore) -> String {
    match expand(template, store).await {
        Ok(expanded) => expanded,
        Err(err) => {
            tracing::warn!(
                error = %err,
                template = template,
                "variable expansion failed; passing through unchanged",
            );
            template.to_string()
        }
    }
}

/// Expand the system instruction in place. The instruction is passed as a
/// borrowed slice so callers (the runner) can clone it before mutation.
pub async fn expand_system_instruction(instruction: &str, store: &VariableStore) -> String {
    expand_string(instruction, store).await
}

/// Return a clone of `tools` with each [`ToolDefinition::description`]
/// expanded. Names and parameter schemas are left untouched — only the
/// human-readable description is treated as a template.
pub async fn expand_tool_descriptions(
    tools: &[ToolDefinition],
    store: &VariableStore,
) -> Vec<ToolDefinition> {
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        let description = expand_string(&tool.description, store).await;
        out.push(ToolDefinition {
            name: tool.name.clone(),
            description,
            parameters: tool.parameters.clone(),
        });
    }
    out
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn
)]
mod tests {
    use super::*;
    use crate::integration::variables::{SessionVariable, VariableSource, VariableStore};

    #[tokio::test]
    async fn expand_system_instruction_substitutes() {
        let store = VariableStore::new();
        store.set(SessionVariable {
            name: "project".to_owned(),
            source: VariableSource::Static {
                value: "norn".to_owned(),
            },
        });
        let out = expand_system_instruction("hello {{project}}", &store).await;
        assert_eq!(out, "hello norn");
    }

    #[tokio::test]
    async fn unknown_variable_passes_through() {
        let store = VariableStore::new();
        let out = expand_system_instruction("hello {{ghost}}", &store).await;
        assert_eq!(out, "hello {{ghost}}");
    }

    #[tokio::test]
    async fn expands_each_tool_description() {
        let store = VariableStore::new();
        store.set(SessionVariable {
            name: "lang".to_owned(),
            source: VariableSource::Static {
                value: "rust".to_owned(),
            },
        });
        let tools = vec![
            ToolDefinition {
                name: "read".to_string(),
                description: "Reads a {{lang}} file".to_string(),
                parameters: serde_json::json!({}),
            },
            ToolDefinition {
                name: "write".to_string(),
                description: "Writes a {{lang}} file".to_string(),
                parameters: serde_json::json!({}),
            },
        ];
        let out = expand_tool_descriptions(&tools, &store).await;
        assert_eq!(out[0].description, "Reads a rust file");
        assert_eq!(out[1].description, "Writes a rust file");
        // Names + parameters untouched.
        assert_eq!(out[0].name, "read");
        assert_eq!(out[1].name, "write");
    }
}
