use serde_json::{Value, json};

pub(crate) use self::tools::public_tool_definitions;
use crate::provider::response_item::{
    ResponseItem, ResponseItemError, ResponseStreamProvenance, ResponseTranscriptItem,
};

mod nested;
mod nested_persistence_tests;
mod reconciler_tests;
mod tests;
mod tools;

/// One schema-complete fixture for each public Responses output-item variant.
pub(crate) fn public_output_item_inventory(id_suffix: &str, text: &str) -> Vec<Value> {
    let tools = public_tool_definitions();
    vec![
        json!({
            "type": "message",
            "id": format!("msg_{id_suffix}"),
            "role": "assistant",
            "phase": "commentary",
            "status": "completed",
            "content": [
                {
                    "type": "output_text",
                    "text": text,
                    "annotations": [
                        {
                            "type": "file_citation",
                            "file_id": "file_inventory",
                            "filename": "inventory.txt",
                            "index": 0
                        },
                        {
                            "type": "url_citation",
                            "start_index": 0,
                            "end_index": text.len(),
                            "url": "https://example.test/inventory",
                            "title": "Inventory source"
                        },
                        {
                            "type": "container_file_citation",
                            "container_id": "container_inventory",
                            "file_id": "file_container_inventory",
                            "filename": "result.txt",
                            "start_index": 0,
                            "end_index": text.len()
                        },
                        {"type": "file_path", "file_id": "file_path_inventory", "index": 1}
                    ],
                    "logprobs": [{
                        "token": text,
                        "bytes": text.as_bytes(),
                        "logprob": -0.1,
                        "top_logprobs": [{
                            "token": "alternate",
                            "bytes": b"alternate",
                            "logprob": -1.2
                        }]
                    }]
                },
                {"type": "refusal", "refusal": "fixture refusal"}
            ]
        }),
        json!({
            "type": "file_search_call",
            "id": format!("fs_{id_suffix}"),
            "queries": ["canonical lifecycle", "response inventory"],
            "status": "completed",
            "results": [{
                "attributes": {"kind": "fixture", "verified": true},
                "file_id": "file_inventory",
                "filename": "inventory.txt",
                "score": 0.99,
                "text": "inventory result"
            }]
        }),
        json!({
            "type": "function_call",
            "id": format!("fc_{id_suffix}"),
            "call_id": format!("call_fc_{id_suffix}"),
            "name": "lookup_record",
            "namespace": "inventory",
            "arguments": "{\"record_id\":\"42\"}",
            "caller": {"type": "direct"},
            "status": "completed"
        }),
        json!({
            "type": "function_call_output",
            "id": format!("fco_{id_suffix}"),
            "call_id": format!("call_fc_{id_suffix}"),
            "output": [
                {"type": "input_text", "text": "record 42"},
                {
                    "type": "input_image",
                    "detail": "high",
                    "image_url": "https://example.test/record.png"
                },
                {
                    "type": "input_file",
                    "file_id": "file_record",
                    "filename": "record.pdf",
                    "detail": "low"
                }
            ],
            "status": "completed",
            "caller": {"type": "direct"},
            "created_by": "fixture"
        }),
        json!({
            "type": "web_search_call",
            "id": format!("ws_{id_suffix}"),
            "status": "completed",
            "action": {
                "type": "search",
                "query": "canonical lifecycle",
                "queries": ["canonical lifecycle", "response replay"],
                "sources": [{"type": "url", "url": "https://example.test/lifecycle"}]
            }
        }),
        json!({
            "type": "computer_call",
            "id": format!("cc_{id_suffix}"),
            "call_id": format!("call_cc_{id_suffix}"),
            "pending_safety_checks": [],
            "status": "completed",
            "action": {"type": "screenshot"}
        }),
        json!({
            "type": "computer_call_output",
            "id": format!("cco_{id_suffix}"),
            "call_id": format!("call_cc_{id_suffix}"),
            "output": {
                "type": "computer_screenshot",
                "file_id": "file_screenshot",
                "image_url": "https://example.test/screenshot.png"
            },
            "status": "completed",
            "acknowledged_safety_checks": [],
            "created_by": "fixture"
        }),
        json!({
            "type": "reasoning",
            "id": format!("rs_{id_suffix}"),
            "summary": [{"type": "summary_text", "text": "preserve canonical order"}],
            "content": [{"type": "reasoning_text", "text": "reasoning detail"}],
            "encrypted_content": "opaque-reasoning",
            "status": "completed"
        }),
        json!({
            "type": "program",
            "id": format!("prog_{id_suffix}"),
            "call_id": format!("call_prog_{id_suffix}"),
            "code": "print('inventory')",
            "fingerprint": "sha256:fixture"
        }),
        json!({
            "type": "program_output",
            "id": format!("progo_{id_suffix}"),
            "call_id": format!("call_prog_{id_suffix}"),
            "result": "inventory\n",
            "status": "completed"
        }),
        json!({
            "type": "tool_search_call",
            "id": format!("ts_{id_suffix}"),
            "arguments": {"query": "inventory tools"},
            "call_id": null,
            "execution": "server",
            "status": "completed",
            "created_by": "fixture"
        }),
        json!({
            "type": "tool_search_output",
            "id": format!("tso_{id_suffix}"),
            "call_id": null,
            "execution": "server",
            "status": "completed",
            "tools": &tools,
            "created_by": "fixture"
        }),
        json!({
            "type": "additional_tools",
            "id": format!("at_{id_suffix}"),
            "role": "assistant",
            "tools": tools
        }),
        json!({
            "type": "compaction",
            "id": format!("cmp_{id_suffix}"),
            "encrypted_content": "opaque-compaction",
            "created_by": "fixture"
        }),
        json!({
            "type": "image_generation_call",
            "id": format!("ig_{id_suffix}"),
            "result": "ZmluYWwtaW1hZ2U=",
            "status": "completed"
        }),
        json!({
            "type": "code_interpreter_call",
            "id": format!("ci_{id_suffix}"),
            "code": "print('ok')",
            "container_id": format!("container_{id_suffix}"),
            "outputs": [
                {"type": "logs", "logs": "ok\n"},
                {"type": "image", "url": "https://example.test/generated.png"}
            ],
            "status": "completed"
        }),
        json!({
            "type": "local_shell_call",
            "id": format!("lsc_{id_suffix}"),
            "call_id": format!("call_lsc_{id_suffix}"),
            "action": {
                "type": "exec",
                "command": ["printf", "inventory"],
                "env": {"LANG": "C"},
                "timeout_ms": 1000,
                "user": null,
                "working_directory": "/work"
            },
            "status": "completed"
        }),
        json!({
            "type": "local_shell_call_output",
            "id": format!("call_lsc_{id_suffix}"),
            "output": "{\"stdout\":\"inventory\",\"stderr\":\"\",\"exit_code\":0}",
            "status": "completed"
        }),
        json!({
            "type": "shell_call",
            "id": format!("sc_{id_suffix}"),
            "call_id": format!("call_sc_{id_suffix}"),
            "action": {
                "commands": ["printf inventory", "pwd"],
                "max_output_length": 4096,
                "timeout_ms": 1000
            },
            "environment": {
                "type": "container_reference",
                "container_id": format!("container_{id_suffix}")
            },
            "status": "completed",
            "caller": {"type": "direct"},
            "created_by": "fixture"
        }),
        json!({
            "type": "shell_call_output",
            "id": format!("sco_{id_suffix}"),
            "call_id": format!("call_sc_{id_suffix}"),
            "max_output_length": 4096,
            "output": [{
                "outcome": {"type": "exit", "exit_code": 0},
                "stderr": "",
                "stdout": "inventory\n/work\n",
                "created_by": "fixture"
            }],
            "status": "completed",
            "caller": {"type": "direct"},
            "created_by": "fixture"
        }),
        json!({
            "type": "apply_patch_call",
            "id": format!("ap_{id_suffix}"),
            "call_id": format!("call_ap_{id_suffix}"),
            "operation": {
                "type": "update_file",
                "path": "inventory.txt",
                "diff": "@@\n-old\n+new"
            },
            "status": "completed",
            "caller": {"type": "direct"},
            "created_by": "fixture"
        }),
        json!({
            "type": "apply_patch_call_output",
            "id": format!("apo_{id_suffix}"),
            "call_id": format!("call_ap_{id_suffix}"),
            "status": "completed",
            "output": "updated inventory.txt",
            "caller": {"type": "direct"},
            "created_by": "fixture"
        }),
        json!({
            "type": "mcp_call",
            "id": format!("mcp_{id_suffix}"),
            "arguments": "{\"query\":\"canonical lifecycle\"}",
            "name": "lookup",
            "server_label": "docs",
            "approval_request_id": null,
            "error": null,
            "output": "structured result",
            "status": "completed"
        }),
        json!({
            "type": "mcp_list_tools",
            "id": format!("mcplt_{id_suffix}"),
            "server_label": "docs",
            "tools": [{
                "name": "lookup",
                "description": "Look up documentation.",
                "input_schema": {
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"]
                },
                "annotations": {"readOnlyHint": true}
            }],
            "error": null
        }),
        json!({
            "type": "mcp_approval_request",
            "id": format!("mcpar_{id_suffix}"),
            "arguments": "{\"query\":\"canonical lifecycle\"}",
            "name": "lookup",
            "server_label": "docs"
        }),
        json!({
            "type": "mcp_approval_response",
            "id": format!("mcpares_{id_suffix}"),
            "approval_request_id": format!("mcpar_{id_suffix}"),
            "approve": true,
            "reason": "fixture approval"
        }),
        json!({
            "type": "custom_tool_call",
            "id": format!("ctc_{id_suffix}"),
            "call_id": format!("call_ctc_{id_suffix}"),
            "name": "freeform_lookup",
            "namespace": "inventory",
            "input": "record 42",
            "caller": {"type": "direct"}
        }),
        json!({
            "type": "custom_tool_call_output",
            "id": format!("ctco_{id_suffix}"),
            "call_id": format!("call_ctc_{id_suffix}"),
            "output": [
                {"type": "input_text", "text": "custom record 42"},
                {"type": "input_image", "detail": "low", "file_id": "file_custom_image"},
                {
                    "type": "input_file",
                    "file_data": "Zml4dHVyZQ==",
                    "filename": "custom.txt"
                }
            ],
            "status": "completed",
            "caller": {"type": "direct"},
            "created_by": "fixture"
        }),
    ]
}

/// Items a live child response can complete without local tool dispatch.
pub(crate) fn spawn_lifecycle_items(id_suffix: &str, text: &str) -> Vec<Value> {
    public_output_item_inventory(id_suffix, text)
        .into_iter()
        .filter(|item| {
            !matches!(
                item.get("type").and_then(Value::as_str),
                Some(
                    "function_call"
                        | "computer_call"
                        | "local_shell_call"
                        | "apply_patch_call"
                        | "mcp_approval_request"
                        | "custom_tool_call"
                )
            )
        })
        .collect()
}

/// Completed history that is safe to seed, reload, and replay into a fork.
pub(crate) fn historical_replay_items(id_suffix: &str, text: &str) -> Vec<Value> {
    public_output_item_inventory(id_suffix, text)
        .into_iter()
        .filter(|item| {
            !matches!(
                item.get("type").and_then(Value::as_str),
                Some(
                    "computer_call"
                        | "local_shell_call"
                        | "apply_patch_call"
                        | "mcp_approval_request"
                )
            )
        })
        .collect()
}

pub(crate) fn response_items_named(
    id_suffix: &str,
    names: &[&str],
) -> Result<Vec<ResponseTranscriptItem>, ResponseItemError> {
    public_output_item_inventory(id_suffix, "canonical resolution")
        .into_iter()
        .filter(|raw| {
            raw.get("type")
                .and_then(Value::as_str)
                .is_some_and(|item_type| names.contains(&item_type))
        })
        .map(|raw| {
            Ok(ResponseTranscriptItem {
                item: ResponseItem::from_value(raw)?,
                provenance: ResponseStreamProvenance::default(),
            })
        })
        .collect()
}
