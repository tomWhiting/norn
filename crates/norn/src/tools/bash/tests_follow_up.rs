#![allow(
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
use super::*;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::follow_up::{Confidence, ExpiryCondition};
use crate::tool::traits::{Tool, ToolOutput};
use serde_json::{Value, json};

fn envelope(args: Value) -> ToolEnvelope {
    ToolEnvelope {
        tool_call_id: "call-bash".to_owned(),
        tool_name: "bash".to_owned(),
        model_args: args,
        metadata: Value::Null,
    }
}

// --- NTF-004 R6: rerun follow-ups ------------------------------------

#[tokio::test]
async fn rerun_follow_ups_registered_after_success() {
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "echo hi" }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    let follow_ups = tool.register_follow_ups(&out, &ToolContext::empty()).await;
    assert_eq!(follow_ups.len(), 2);

    let rerun = follow_ups
        .iter()
        .find(|f| f.action == "rerun")
        .expect("rerun present");
    assert_eq!(rerun.tool, "bash");
    assert_eq!(rerun.args, json!({}), "rerun reuses original args");
    assert!(matches!(rerun.expires, ExpiryCondition::Never));

    let timed = follow_ups
        .iter()
        .find(|f| f.action == "rerun_with_timeout")
        .expect("rerun_with_timeout present");
    // Default timeout is 120s → doubled to 240.
    assert_eq!(timed.args, json!({ "timeout": 240 }));
    assert!(matches!(timed.expires, ExpiryCondition::Never));
}

#[tokio::test]
async fn rerun_with_timeout_doubles_explicit_timeout() {
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "echo hi", "timeout": 5_u64 }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    let follow_ups = tool.register_follow_ups(&out, &ToolContext::empty()).await;
    let timed = follow_ups
        .iter()
        .find(|f| f.action == "rerun_with_timeout")
        .expect("rerun_with_timeout present");
    assert_eq!(timed.args, json!({ "timeout": 10 }));
}

#[tokio::test]
async fn rerun_follow_ups_registered_after_failure() {
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "exit 7" }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    assert!(out.is_error());
    let follow_ups = tool.register_follow_ups(&out, &ToolContext::empty()).await;
    let actions: Vec<&str> = follow_ups.iter().map(|f| f.action.as_str()).collect();
    assert!(actions.contains(&"rerun"));
    assert!(actions.contains(&"rerun_with_timeout"));
}

#[tokio::test]
async fn redirected_output_registers_read_and_grep_follow_ups() {
    let tool = BashTool::new();
    let out = ToolOutput::success(json!({
        "exit_code": 0,
        "output_redirected": true,
        "output_path": "~/.norn/outputs/session/call-bash.log",
        "output_chars": 23_001,
        "timed_out": false,
        "metadata": { "risk_tier": "Harmless", "timeout_secs": 120 }
    }));

    let follow_ups = tool.register_follow_ups(&out, &ToolContext::empty()).await;
    let read = follow_ups
        .iter()
        .find(|f| f.action == "read_output")
        .expect("read_output present");
    assert_eq!(read.tool, "read");
    assert_eq!(
        read.args,
        json!({ "path": "~/.norn/outputs/session/call-bash.log", "offset": 1, "limit": 200 })
    );
    assert!(matches!(read.expires, ExpiryCondition::Never));
    assert!(matches!(read.confidence, Confidence::High));

    let grep = follow_ups
        .iter()
        .find(|f| f.action == "grep_output")
        .expect("grep_output present");
    assert_eq!(grep.tool, "search");
    assert_eq!(
        grep.args,
        json!({ "path": "~/.norn/outputs/session/call-bash.log", "mode": "content" })
    );
    assert!(matches!(grep.expires, ExpiryCondition::Never));
    assert!(matches!(grep.confidence, Confidence::Medium));
}

// --- NP-001 R7: background / migrated results target the process tool ------

#[tokio::test]
async fn backgrounded_result_registers_process_follow_ups() {
    let tool = BashTool::new();
    let out = ToolOutput::success(json!({
        "background": true,
        "process_id": "p1",
        "spool_path": "~/.norn/outputs/sess/processes/p1.log",
        "command": "npm run dev",
        "hint": "Started in the background as process p1.",
        "metadata": { "risk_tier": "Harmless" }
    }));
    let follow_ups = tool.register_follow_ups(&out, &ToolContext::empty()).await;
    let actions: Vec<&str> = follow_ups.iter().map(|f| f.action.as_str()).collect();
    assert_eq!(
        actions,
        vec!["check_output", "kill_process"],
        "backgrounded results replace the rerun set with the process ops",
    );

    let check = follow_ups
        .iter()
        .find(|f| f.action == "check_output")
        .expect("check_output present");
    assert_eq!(check.tool, "process");
    assert_eq!(check.args, json!({ "op": "output", "id": "p1" }));
    assert!(matches!(
        check.args_mode,
        crate::tool::follow_up::FollowUpArgsMode::Replace
    ));
    assert!(matches!(check.expires, ExpiryCondition::Never));
    assert!(matches!(check.confidence, Confidence::High));

    let kill = follow_ups
        .iter()
        .find(|f| f.action == "kill_process")
        .expect("kill_process present");
    assert_eq!(kill.tool, "process");
    assert_eq!(kill.args, json!({ "op": "kill", "id": "p1" }));
    assert!(matches!(kill.confidence, Confidence::Medium));
}

#[tokio::test]
async fn migrated_result_registers_process_follow_ups_bound_to_the_process() {
    let tool = BashTool::new();
    let out = ToolOutput::success(json!({
        "migrated": true,
        "process_id": "p7",
        "spool_path": "~/.norn/outputs/sess/processes/p7.log",
        "command": "cargo test",
        "stdout": "running 200 tests\n",
        "stderr": "",
        "hint": "moved to the background as process p7",
        "metadata": { "risk_tier": "Harmless", "timeout_secs": 120 }
    }));
    let follow_ups = tool.register_follow_ups(&out, &ToolContext::empty()).await;
    let actions: Vec<&str> = follow_ups.iter().map(|f| f.action.as_str()).collect();
    assert_eq!(actions, vec!["check_output", "kill_process"]);
    let check = follow_ups
        .iter()
        .find(|f| f.action == "check_output")
        .expect("check_output present");
    assert_eq!(check.args, json!({ "op": "output", "id": "p7" }));
}

#[tokio::test]
async fn foreground_result_keeps_rerun_set_and_no_process_ops() {
    let tool = BashTool::new();
    let out = tool
        .execute(
            &envelope(json!({ "command": "echo hi" })),
            &ToolContext::empty(),
        )
        .await
        .expect("bash ok");
    let follow_ups = tool.register_follow_ups(&out, &ToolContext::empty()).await;
    let actions: Vec<&str> = follow_ups.iter().map(|f| f.action.as_str()).collect();
    assert!(actions.contains(&"rerun"));
    assert!(actions.contains(&"rerun_with_timeout"));
    assert!(
        !actions.contains(&"check_output"),
        "no process ops on a foreground result"
    );
}

#[tokio::test]
async fn registry_dispatches_bash_tool() {
    use std::sync::Arc;

    use crate::r#loop::runner::ToolExecutor;
    use crate::tool::registry::ToolRegistry;

    let ctx = Arc::new(ToolContext::empty());
    let mut reg = ToolRegistry::with_context(Arc::clone(&ctx));
    reg.register(Box::new(BashTool::new()));
    let executor: &dyn ToolExecutor = &reg;

    let out = executor
        .execute("bash", "test-call", json!({ "command": "echo hi" }))
        .await
        .expect("dispatch succeeds");
    // The tool's stdout field must carry the command's output.
    let stdout = out["stdout"].as_str().unwrap_or("");
    assert!(
        stdout.contains("hi"),
        "expected stdout to contain 'hi', got: {stdout}"
    );
}
