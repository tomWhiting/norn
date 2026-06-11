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
use crate::tool::envelope::{RuntimeInputs, ToolEnvelope};
use crate::tool::follow_up::{Confidence, ExpiryCondition};
use crate::tool::traits::{Tool, ToolOutput};
use serde_json::{Value, json};

fn envelope(args: Value) -> ToolEnvelope {
    ToolEnvelope {
        tool_call_id: "call-bash".to_owned(),
        tool_name: "bash".to_owned(),
        model_args: args,
        runtime_inputs: RuntimeInputs::default(),
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
