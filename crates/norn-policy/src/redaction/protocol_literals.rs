macro_rules! closed_literals {
    ($function:ident, $values:ident, $first:literal $(| $rest:literal)*) => {
        const $values: &[&str] = &[$first, $($rest),*];

        pub(crate) fn $function(value: &str) -> bool {
            $values.contains(&value)
        }
    };
}

closed_literals!(
    type_accepts,
    TYPE_LITERALS,
    "additional_tools"
        | "allowed_tools"
        | "allowlist"
        | "and"
        | "apply_patch"
        | "apply_patch_call"
        | "apply_patch_call_output"
        | "approximate"
        | "auto"
        | "base64"
        | "click"
        | "code_interpreter"
        | "code_interpreter_call"
        | "compaction"
        | "compaction_trigger"
        | "computer"
        | "computer_call"
        | "computer_call_output"
        | "computer_screenshot"
        | "computer_use"
        | "computer_use_preview"
        | "container_auto"
        | "container_file_citation"
        | "container_reference"
        | "create_file"
        | "custom"
        | "custom_tool_call"
        | "custom_tool_call_output"
        | "delete_file"
        | "direct"
        | "disabled"
        | "double_click"
        | "drag"
        | "eq"
        | "error"
        | "exec"
        | "exit"
        | "file_citation"
        | "file_path"
        | "file_search"
        | "file_search_call"
        | "find_in_page"
        | "function"
        | "function_call"
        | "function_call_output"
        | "grammar"
        | "gt"
        | "gte"
        | "image"
        | "image_generation"
        | "image_generation_call"
        | "in"
        | "inline"
        | "input_file"
        | "input_image"
        | "input_text"
        | "item_reference"
        | "json_object"
        | "json_schema"
        | "keypress"
        | "local"
        | "local_shell"
        | "local_shell_call"
        | "local_shell_call_output"
        | "logs"
        | "lt"
        | "lte"
        | "mcp"
        | "mcp_approval_request"
        | "mcp_approval_response"
        | "mcp_call"
        | "mcp_list_tools"
        | "message"
        | "moderation_result"
        | "move"
        | "namespace"
        | "ne"
        | "nin"
        | "open_page"
        | "or"
        | "output_text"
        | "program"
        | "program_output"
        | "programmatic_tool_calling"
        | "reasoning"
        | "reasoning_text"
        | "refusal"
        | "response.create"
        | "response.created"
        | "screenshot"
        | "scroll"
        | "search"
        | "shell"
        | "shell_call"
        | "shell_call_output"
        | "skill_reference"
        | "summary_text"
        | "text"
        | "timeout"
        | "tool_search"
        | "tool_search_call"
        | "tool_search_output"
        | "type"
        | "update_file"
        | "url"
        | "url_citation"
        | "wait"
        | "web_search"
        | "web_search_2025_08_26"
        | "web_search_call"
        | "web_search_preview"
        | "web_search_preview_2025_03_11"
        | "response.audio.delta"
        | "response.audio.done"
        | "response.audio.transcript.delta"
        | "response.audio.transcript.done"
        | "response.code_interpreter_call.completed"
        | "response.code_interpreter_call.in_progress"
        | "response.code_interpreter_call.interpreting"
        | "response.code_interpreter_call_code.delta"
        | "response.code_interpreter_call_code.done"
        | "response.completed"
        | "response.content_part.added"
        | "response.content_part.done"
        | "response.custom_tool_call_input.delta"
        | "response.custom_tool_call_input.done"
        | "response.failed"
        | "response.file_search_call.completed"
        | "response.file_search_call.in_progress"
        | "response.file_search_call.searching"
        | "response.function_call_arguments.delta"
        | "response.function_call_arguments.done"
        | "response.image_generation_call.completed"
        | "response.image_generation_call.generating"
        | "response.image_generation_call.in_progress"
        | "response.image_generation_call.partial_image"
        | "response.in_progress"
        | "response.incomplete"
        | "response.mcp_call.completed"
        | "response.mcp_call.failed"
        | "response.mcp_call.in_progress"
        | "response.mcp_call_arguments.delta"
        | "response.mcp_call_arguments.done"
        | "response.mcp_list_tools.completed"
        | "response.mcp_list_tools.failed"
        | "response.mcp_list_tools.in_progress"
        | "response.output_item.added"
        | "response.output_item.done"
        | "response.output_text.annotation.added"
        | "response.output_text.delta"
        | "response.output_text.done"
        | "response.queued"
        | "response.reasoning_summary_part.added"
        | "response.reasoning_summary_part.done"
        | "response.reasoning_summary_text.delta"
        | "response.reasoning_summary_text.done"
        | "response.reasoning_text.delta"
        | "response.reasoning_text.done"
        | "response.refusal.delta"
        | "response.refusal.done"
        | "response.web_search_call.completed"
        | "response.web_search_call.in_progress"
        | "response.web_search_call.searching"
);

closed_literals!(
    include_accepts,
    INCLUDE_LITERALS,
    "file_search_call.results"
        | "web_search_call.results"
        | "web_search_call.action.sources"
        | "message.input_image.image_url"
        | "computer_call_output.output.image_url"
        | "code_interpreter_call.outputs"
        | "reasoning.encrypted_content"
        | "message.output_text.logprobs"
);

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::error::Error;
    use std::io;

    use serde_json::Value;

    use super::{INCLUDE_LITERALS, TYPE_LITERALS, include_accepts, type_accepts};

    const INVENTORIES: &str =
        include_str!("../../../../policy/contracts/openai-responses-v1/inventories.json");
    const REQUEST_GRAPH: &str =
        include_str!("../../../../policy/contracts/openai-responses-v1/request-graph.json");
    const RESPONSE_GRAPH: &str =
        include_str!("../../../../policy/contracts/openai-responses-v1/response-graph.json");
    const SSE_EVENTS: &str =
        include_str!("../../../../policy/contracts/openai-responses-v1/sse-events.json");

    #[test]
    fn accepts_every_discriminator_in_the_pinned_contract() -> Result<(), Box<dyn Error>> {
        let inventories = serde_json::from_str::<Value>(INVENTORIES)?;
        let mut expected = BTreeSet::new();
        collect_accepted_literals(&inventories, &mut expected)?;

        let events = serde_json::from_str::<Value>(SSE_EVENTS)?;
        let events = array_at(&events, "events")?;
        if events.len() != 53 {
            return Err(io::Error::other("pinned SSE inventory size changed").into());
        }
        for event in events {
            let event = string_at(event, "event")?;
            expected.insert(event.to_owned());
        }

        collect_graph_literals(REQUEST_GRAPH, &mut expected)?;
        collect_graph_literals(RESPONSE_GRAPH, &mut expected)?;
        let actual = TYPE_LITERALS
            .iter()
            .map(|literal| (*literal).to_owned())
            .collect::<BTreeSet<_>>();
        if actual != expected || actual.len() != TYPE_LITERALS.len() {
            return Err(io::Error::other("closed discriminator set differs from pins").into());
        }
        if !actual.iter().all(|literal| type_accepts(literal)) {
            return Err(io::Error::other("closed discriminator matcher is inconsistent").into());
        }
        Ok(())
    }

    #[test]
    fn accepts_exactly_the_pinned_include_inventory() -> Result<(), Box<dyn Error>> {
        let inventories = serde_json::from_str::<Value>(INVENTORIES)?;
        let includes = array_at(&inventories, "include_values")?;
        if includes.len() != 8 {
            return Err(io::Error::other("pinned include inventory size changed").into());
        }
        let mut expected = BTreeSet::new();
        for include in includes {
            let include = include
                .as_str()
                .ok_or_else(|| io::Error::other("pinned include is not a string"))?;
            expected.insert(include.to_owned());
        }
        let actual = INCLUDE_LITERALS
            .iter()
            .map(|literal| (*literal).to_owned())
            .collect::<BTreeSet<_>>();
        if actual != expected || actual.len() != INCLUDE_LITERALS.len() {
            return Err(io::Error::other("closed include set differs from pins").into());
        }
        if !actual.iter().all(|include| include_accepts(include)) {
            return Err(io::Error::other("closed include matcher is inconsistent").into());
        }
        Ok(())
    }

    fn collect_accepted_literals(
        value: &Value,
        expected: &mut BTreeSet<String>,
    ) -> Result<(), Box<dyn Error>> {
        match value {
            Value::Array(values) => {
                for value in values {
                    collect_accepted_literals(value, expected)?;
                }
            }
            Value::Object(object) => {
                if let Some(literals) = object.get("accepted_literals") {
                    let literals = literals
                        .as_array()
                        .ok_or_else(|| io::Error::other("accepted_literals is not an array"))?;
                    for literal in literals {
                        let literal = literal
                            .as_str()
                            .ok_or_else(|| io::Error::other("accepted literal is not a string"))?;
                        expected.insert(literal.to_owned());
                    }
                }
                for child in object.values() {
                    collect_accepted_literals(child, expected)?;
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
        Ok(())
    }

    fn collect_graph_literals(
        document: &str,
        expected: &mut BTreeSet<String>,
    ) -> Result<(), Box<dyn Error>> {
        let graph = serde_json::from_str::<Value>(document)?;
        for node in array_at(&graph, "nodes")? {
            let source_key = string_at(node, "source_key")?;
            if source_key.contains("(property) type") {
                let declaration = node
                    .get("declaration")
                    .ok_or_else(|| io::Error::other("type node has no declaration"))?;
                collect_literal_nodes(declaration, expected)?;
            }
        }
        Ok(())
    }

    fn collect_literal_nodes(
        value: &Value,
        expected: &mut BTreeSet<String>,
    ) -> Result<(), Box<dyn Error>> {
        match value {
            Value::Array(values) => {
                for value in values {
                    collect_literal_nodes(value, expected)?;
                }
            }
            Value::Object(object) => {
                if let Some(literal) = object.get("literal") {
                    let literal = literal
                        .as_str()
                        .ok_or_else(|| io::Error::other("graph literal is not a string"))?;
                    expected.insert(literal.to_owned());
                }
                for child in object.values() {
                    collect_literal_nodes(child, expected)?;
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
        Ok(())
    }

    fn array_at<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], io::Error> {
        value
            .get(key)
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .ok_or_else(|| io::Error::other("pinned array is missing"))
    }

    fn string_at<'a>(value: &'a Value, key: &str) -> Result<&'a str, io::Error> {
        value
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| io::Error::other("pinned string is missing"))
    }
}
