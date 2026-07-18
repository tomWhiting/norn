use serde_json::{Value, json};

/// One valid fixture per public output item with every optional field absent.
pub(crate) fn minimal_output_item_inventory(id_suffix: &str) -> Vec<Value> {
    vec![
        json!({
            "type": "message",
            "id": format!("msg_min_{id_suffix}"),
            "content": [],
            "role": "assistant",
            "status": "completed"
        }),
        json!({
            "type": "file_search_call",
            "id": format!("fs_min_{id_suffix}"),
            "queries": [],
            "status": "completed"
        }),
        json!({
            "type": "function_call",
            "arguments": "{}",
            "call_id": format!("call_fc_min_{id_suffix}"),
            "name": "minimal_function"
        }),
        json!({
            "type": "function_call_output",
            "id": format!("fco_min_{id_suffix}"),
            "call_id": format!("call_fc_min_{id_suffix}"),
            "output": "minimal function output",
            "status": "completed"
        }),
        json!({
            "type": "web_search_call",
            "id": format!("ws_min_{id_suffix}"),
            "action": {"type": "search"},
            "status": "completed"
        }),
        json!({
            "type": "computer_call",
            "id": format!("cc_min_{id_suffix}"),
            "call_id": format!("call_cc_min_{id_suffix}"),
            "pending_safety_checks": [],
            "status": "completed"
        }),
        json!({
            "type": "computer_call_output",
            "id": format!("cco_min_{id_suffix}"),
            "call_id": format!("call_cc_min_{id_suffix}"),
            "output": {"type": "computer_screenshot"},
            "status": "completed"
        }),
        json!({
            "type": "reasoning",
            "id": format!("rs_min_{id_suffix}"),
            "summary": []
        }),
        json!({
            "type": "program",
            "id": format!("prog_min_{id_suffix}"),
            "call_id": format!("call_prog_min_{id_suffix}"),
            "code": "",
            "fingerprint": "minimal"
        }),
        json!({
            "type": "program_output",
            "id": format!("progo_min_{id_suffix}"),
            "call_id": format!("call_prog_min_{id_suffix}"),
            "result": "",
            "status": "completed"
        }),
        json!({
            "type": "tool_search_call",
            "id": format!("ts_min_{id_suffix}"),
            "arguments": {},
            "call_id": null,
            "execution": "server",
            "status": "completed"
        }),
        json!({
            "type": "tool_search_output",
            "id": format!("tso_min_{id_suffix}"),
            "call_id": null,
            "execution": "server",
            "status": "completed",
            "tools": []
        }),
        json!({
            "type": "additional_tools",
            "id": format!("at_min_{id_suffix}"),
            "role": "assistant",
            "tools": []
        }),
        json!({
            "type": "compaction",
            "id": format!("cmp_min_{id_suffix}"),
            "encrypted_content": "minimal-compaction"
        }),
        json!({
            "type": "image_generation_call",
            "id": format!("ig_min_{id_suffix}"),
            "result": null,
            "status": "completed"
        }),
        json!({
            "type": "code_interpreter_call",
            "id": format!("ci_min_{id_suffix}"),
            "code": null,
            "container_id": format!("container_min_{id_suffix}"),
            "outputs": null,
            "status": "completed"
        }),
        json!({
            "type": "local_shell_call",
            "id": format!("lsc_min_{id_suffix}"),
            "action": {"type": "exec", "command": [], "env": {}},
            "call_id": format!("call_lsc_min_{id_suffix}"),
            "status": "completed"
        }),
        json!({
            "type": "local_shell_call_output",
            "id": format!("lsco_min_{id_suffix}"),
            "output": ""
        }),
        json!({
            "type": "shell_call",
            "id": format!("sc_min_{id_suffix}"),
            "action": {"commands": [], "max_output_length": null, "timeout_ms": null},
            "call_id": format!("call_sc_min_{id_suffix}"),
            "environment": null,
            "status": "completed"
        }),
        json!({
            "type": "shell_call_output",
            "id": format!("sco_min_{id_suffix}"),
            "call_id": format!("call_sc_min_{id_suffix}"),
            "max_output_length": null,
            "output": [],
            "status": "completed"
        }),
        json!({
            "type": "apply_patch_call",
            "id": format!("ap_min_{id_suffix}"),
            "call_id": format!("call_ap_min_{id_suffix}"),
            "operation": {"type": "create_file", "path": "minimal.txt", "diff": ""},
            "status": "completed"
        }),
        json!({
            "type": "apply_patch_call_output",
            "id": format!("apo_min_{id_suffix}"),
            "call_id": format!("call_ap_min_{id_suffix}"),
            "status": "completed"
        }),
        json!({
            "type": "mcp_call",
            "id": format!("mcp_min_{id_suffix}"),
            "arguments": "{}",
            "name": "minimal_mcp",
            "server_label": "minimal"
        }),
        json!({
            "type": "mcp_list_tools",
            "id": format!("mcplt_min_{id_suffix}"),
            "server_label": "minimal",
            "tools": []
        }),
        json!({
            "type": "mcp_approval_request",
            "id": format!("mcpar_min_{id_suffix}"),
            "arguments": "{}",
            "name": "minimal_mcp",
            "server_label": "minimal"
        }),
        json!({
            "type": "mcp_approval_response",
            "id": format!("mcpares_min_{id_suffix}"),
            "approval_request_id": format!("mcpar_min_{id_suffix}"),
            "approve": false
        }),
        json!({
            "type": "custom_tool_call",
            "call_id": format!("call_ctc_min_{id_suffix}"),
            "input": "minimal custom input",
            "name": "minimal_custom"
        }),
        json!({
            "type": "custom_tool_call_output",
            "id": format!("ctco_min_{id_suffix}"),
            "call_id": format!("call_ctc_min_{id_suffix}"),
            "output": "minimal custom output",
            "status": "completed"
        }),
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::provider::openai::response_contract::PUBLIC_OUTPUT_ITEMS;

    #[test]
    fn minimal_inventory_covers_every_public_item_with_exact_required_keys() {
        let inventory = minimal_output_item_inventory("keys");
        let expected_keys: [&[&str]; 28] = [
            &["content", "id", "role", "status", "type"],
            &["id", "queries", "status", "type"],
            &["arguments", "call_id", "name", "type"],
            &["call_id", "id", "output", "status", "type"],
            &["action", "id", "status", "type"],
            &["call_id", "id", "pending_safety_checks", "status", "type"],
            &["call_id", "id", "output", "status", "type"],
            &["id", "summary", "type"],
            &["call_id", "code", "fingerprint", "id", "type"],
            &["call_id", "id", "result", "status", "type"],
            &["arguments", "call_id", "execution", "id", "status", "type"],
            &["call_id", "execution", "id", "status", "tools", "type"],
            &["id", "role", "tools", "type"],
            &["encrypted_content", "id", "type"],
            &["id", "result", "status", "type"],
            &["code", "container_id", "id", "outputs", "status", "type"],
            &["action", "call_id", "id", "status", "type"],
            &["id", "output", "type"],
            &["action", "call_id", "environment", "id", "status", "type"],
            &[
                "call_id",
                "id",
                "max_output_length",
                "output",
                "status",
                "type",
            ],
            &["call_id", "id", "operation", "status", "type"],
            &["call_id", "id", "status", "type"],
            &["arguments", "id", "name", "server_label", "type"],
            &["id", "server_label", "tools", "type"],
            &["arguments", "id", "name", "server_label", "type"],
            &["approval_request_id", "approve", "id", "type"],
            &["call_id", "input", "name", "type"],
            &["call_id", "id", "output", "status", "type"],
        ];

        assert_eq!(inventory.len(), PUBLIC_OUTPUT_ITEMS.len());
        for ((item, entry), expected) in
            inventory.iter().zip(PUBLIC_OUTPUT_ITEMS).zip(expected_keys)
        {
            assert_eq!(item.get("type").and_then(Value::as_str), Some(entry.name()));
            let actual = item
                .as_object()
                .into_iter()
                .flat_map(serde_json::Map::keys)
                .map(String::as_str)
                .collect::<BTreeSet<_>>();
            assert_eq!(
                actual,
                expected.iter().copied().collect(),
                "{}",
                entry.name()
            );
        }
    }

    #[test]
    fn minimal_inventory_retains_applicable_required_nullable_fields_as_null() {
        let inventory = minimal_output_item_inventory("nulls");
        for (index, pointer) in [
            (10, "/call_id"),
            (11, "/call_id"),
            (14, "/result"),
            (15, "/code"),
            (15, "/outputs"),
            (18, "/action/max_output_length"),
            (18, "/action/timeout_ms"),
            (18, "/environment"),
            (19, "/max_output_length"),
        ] {
            assert_eq!(inventory[index].pointer(pointer), Some(&Value::Null));
        }
    }
}
