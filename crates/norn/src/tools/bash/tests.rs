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
use crate::tool::context::{SessionId, ToolContext};
use crate::tool::envelope::{RuntimeInputs, ToolEnvelope};
use crate::tool::risk::BashRiskTier;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

fn envelope(args: Value) -> ToolEnvelope {
    ToolEnvelope {
        tool_call_id: "call-bash".to_owned(),
        tool_name: "bash".to_owned(),
        model_args: args,
        runtime_inputs: RuntimeInputs::default(),
        metadata: Value::Null,
    }
}

fn expand_tilde_for_test(path: &str) -> std::path::PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir().expect("home dir").join(rest)
    } else {
        std::path::PathBuf::from(path)
    }
}

#[test]
fn bash_args_schema_matches_previous_hand_written_schema() {
    let expected_schema = json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "description": "Shell command line. Executed via `sh -c`."
            },
            "timeout": {
                "type": "integer",
                "minimum": 0,
                "description": "Timeout in seconds. 0 means wait forever. Defaults to 120."
            },
            "working_dir": {
                "type": "string",
                "description": "Working directory for the subprocess."
            }
        },
        "required": ["command"],
        "additionalProperties": false
    });
    assert_eq!(BashArgs::json_schema(), expected_schema);
}

#[test]
fn object_safe() {
    let _: Box<dyn Tool + Send + Sync> = Box::new(BashTool::new());
}

#[test]
fn effect_is_process() {
    assert_eq!(BashTool::new().effect(), ToolEffect::Process);
}

#[test]
fn name_is_bash() {
    assert_eq!(BashTool::new().name(), "bash");
}

#[tokio::test]
async fn echo_hello_succeeds() {
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "echo hello" }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");

    assert!(!out.is_error);
    assert_eq!(out.content["exit_code"].as_i64(), Some(0));
    let stdout = out.content["stdout"].as_str().unwrap_or_default();
    assert!(stdout.contains("hello"), "stdout was {stdout:?}");
    assert_eq!(
        out.content["metadata"]["risk_tier"].as_str(),
        Some("Harmless"),
    );
}

#[tokio::test]
async fn output_under_threshold_returns_inline() {
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "printf small-output" }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");

    assert_eq!(out.content["stdout"].as_str(), Some("small-output\n"));
    assert_eq!(out.content["stderr"].as_str(), Some(""));
    assert!(out.content.get("output_redirected").is_none());
}

#[tokio::test]
async fn output_over_threshold_redirects_to_file_with_shape_and_content() {
    let ctx = ToolContext::empty();
    let session_id = format!("bash-test-{}", Uuid::new_v4());
    ctx.insert_extension(Arc::new(SessionId(session_id.clone())));
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": "yes O | head -c 23000; printf '\\n'; printf 'ERR\\n' 1>&2",
    }));

    let out = tool.execute(&env, &ctx).await.expect("bash ok");

    assert_eq!(out.content["output_redirected"].as_bool(), Some(true));
    assert_eq!(out.content["output_chars"].as_u64(), Some(23_005));
    assert!(out.content.get("stdout_chars").is_none());
    assert!(out.content.get("stderr_chars").is_none());
    let path = out.content["output_path"].as_str().expect("output_path");
    assert!(path.contains(&session_id));
    assert!(path.ends_with("call-bash.log"));
    assert!(
        out.content["hint"]
            .as_str()
            .expect("hint")
            .contains("22000-character inline threshold")
    );

    let absolute = expand_tilde_for_test(path);
    let content = tokio::fs::read_to_string(&absolute)
        .await
        .expect("read log");
    assert!(content.contains("O\nO\nO\n"));
    assert!(content.len() > 23_000);
    assert!(content.contains("ERR\n"));
    assert!(!content.contains("=== STDOUT ==="));
    assert!(!content.contains("=== STDERR ==="));
    assert!(absolute.parent().expect("session dir").is_dir());
}

#[tokio::test]
async fn timeout_with_partial_output_redirects_to_file() {
    let ctx = ToolContext::empty();
    let session_id = format!("bash-timeout-test-{}", Uuid::new_v4());
    ctx.insert_extension(Arc::new(SessionId(session_id)));
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": "yes P | head -c 23000; printf '\\n'; sleep 5",
        "timeout": 1_u64,
    }));

    let out = tool.execute(&env, &ctx).await.expect("bash ok");

    assert!(out.is_error);
    assert_eq!(out.content["timed_out"].as_bool(), Some(true));
    assert_eq!(out.content["output_redirected"].as_bool(), Some(true));
    assert!(out.content["output_chars"].as_u64().unwrap_or_default() >= 23_001);
    assert!(
        out.content["hint"]
            .as_str()
            .expect("hint")
            .contains("Command timed out after 1s")
    );
    let path = out.content["output_path"].as_str().expect("output_path");
    let content = tokio::fs::read_to_string(expand_tilde_for_test(path))
        .await
        .expect("read log");
    assert!(content.contains("P\nP\nP\n"));
    assert!(content.len() > 23_000);
}

#[tokio::test]
async fn non_zero_exit_marks_is_error() {
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "exit 7" }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    assert!(out.is_error);
    assert_eq!(out.content["exit_code"].as_i64(), Some(7));
}

#[tokio::test]
async fn stderr_captured_separately() {
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "echo oops 1>&2" }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    assert_eq!(out.content["exit_code"].as_i64(), Some(0));
    assert!(
        out.content["stderr"]
            .as_str()
            .unwrap_or("")
            .contains("oops")
    );
}

#[tokio::test]
async fn timeout_kills_long_running_process() {
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": "sleep 5",
        "timeout": 1_u64,
    }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    assert!(out.is_error);
    assert_eq!(out.content["timed_out"].as_bool(), Some(true));
}

#[tokio::test]
async fn working_dir_is_applied() {
    let dir = tempfile::tempdir().expect("tempdir");
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": "pwd",
        "working_dir": dir.path().to_string_lossy(),
    }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    let stdout = out.content["stdout"].as_str().unwrap_or_default().trim();
    // On macOS, /tmp resolves to /private/tmp; compare suffix.
    let expected = dir.path().to_string_lossy();
    assert!(
        stdout.ends_with(expected.as_ref()),
        "expected pwd {stdout:?} to end with {expected:?}",
    );
}

#[tokio::test]
async fn critical_risk_tier_appears_in_metadata() {
    // `classify_risk` flags any command line containing the literal
    // substring `chmod 777` as Critical. The executed shell only echoes
    // the string to /dev/null — no actual permission change occurs — so
    // the tier assertion is meaningful and the test is side-effect-free.
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": "echo 'chmod 777 nothing' > /dev/null",
    }));
    let out = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect("bash ok");
    assert_eq!(
        out.content["metadata"]["risk_tier"].as_str(),
        Some("Critical"),
    );
}

#[tokio::test]
async fn pre_validate_proceeds_on_critical_command() {
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "sudo rm -rf /" }));
    let outcome = tool.pre_validate(&env, &ToolContext::empty()).await;
    assert!(matches!(outcome, PreValidateOutcome::Proceed));
    // And classify_risk confirms the tier independently.
    assert_eq!(classify_risk("sudo rm -rf /"), BashRiskTier::Critical);
    assert_eq!(classify_risk("ls"), BashRiskTier::Harmless);
}

#[tokio::test]
async fn invalid_args_pre_validation_failure() {
    let tool = BashTool::new();
    let env = envelope(json!({ "not_command": "oops" }));
    let err = tool
        .execute(&env, &ToolContext::empty())
        .await
        .expect_err("must fail");
    assert!(matches!(err, ToolError::PreValidationFailed { .. }));
}

#[tokio::test]
async fn ctx_working_dir_used_when_args_working_dir_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let canonical = dir.path().canonicalize().expect("canonicalize");
    let ctx = ToolContext::with_working_dir(crate::tool::context::SharedWorkingDir::new(
        canonical.clone(),
    ));
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "pwd" }));
    let out = tool.execute(&env, &ctx).await.expect("bash ok");
    let stdout = out.content["stdout"].as_str().unwrap_or_default().trim();
    assert_eq!(stdout, canonical.to_string_lossy());
}

#[tokio::test]
async fn cd_absolute_updates_working_dir() {
    let dir = tempfile::tempdir().expect("tempdir");
    let canonical = dir.path().canonicalize().expect("canonicalize");
    let ctx = ToolContext::empty();
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": format!("cd {}", canonical.display()),
    }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(ctx.working_dir(), canonical);
}

#[tokio::test]
async fn cd_relative_joins_against_working_dir() {
    let root = tempfile::tempdir().expect("tempdir");
    let sub = root.path().join("sub");
    std::fs::create_dir(&sub).expect("mkdir sub");
    let root_canonical = root.path().canonicalize().expect("canonicalize root");
    let sub_canonical = sub.canonicalize().expect("canonicalize sub");
    let ctx =
        ToolContext::with_working_dir(crate::tool::context::SharedWorkingDir::new(root_canonical));
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "cd sub" }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(ctx.working_dir(), sub_canonical);
}

#[tokio::test]
async fn cd_parent_moves_up_one_level() {
    let root = tempfile::tempdir().expect("tempdir");
    let sub = root.path().join("sub");
    std::fs::create_dir(&sub).expect("mkdir sub");
    let root_canonical = root.path().canonicalize().expect("canonicalize root");
    let sub_canonical = sub.canonicalize().expect("canonicalize sub");
    let ctx =
        ToolContext::with_working_dir(crate::tool::context::SharedWorkingDir::new(sub_canonical));
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "cd .." }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(ctx.working_dir(), root_canonical);
}

#[tokio::test]
async fn cd_compound_command_detected() {
    // `ls && cd <dir>` — cd is at the end of a chain; must still be detected.
    let dir = tempfile::tempdir().expect("tempdir");
    let canonical = dir.path().canonicalize().expect("canonicalize");
    let ctx = ToolContext::empty();
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": format!("ls > /dev/null && cd {}", canonical.display()),
    }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(ctx.working_dir(), canonical);
}

#[tokio::test]
async fn cd_prefix_in_chain_detected() {
    // `cd <dir> && ls` — cd is at the start with `&&` separator.
    let dir = tempfile::tempdir().expect("tempdir");
    let canonical = dir.path().canonicalize().expect("canonicalize");
    let ctx = ToolContext::empty();
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": format!("cd {} && ls > /dev/null", canonical.display()),
    }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(ctx.working_dir(), canonical);
}

#[tokio::test]
async fn cd_to_nonexistent_dir_does_not_update() {
    let ctx = ToolContext::with_working_dir(crate::tool::context::SharedWorkingDir::new(
        std::path::PathBuf::from("/tmp"),
    ));
    let original = ctx.working_dir();
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": "cd /no/such/path/should/exist || true",
    }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(ctx.working_dir(), original);
}

#[tokio::test]
async fn cd_with_tilde_expands_to_home() {
    let home = dirs::home_dir().expect("home dir");
    let ctx = ToolContext::empty();
    let tool = BashTool::new();
    let env = envelope(json!({ "command": "cd ~" }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    let expected = home.canonicalize().unwrap_or(home);
    assert_eq!(ctx.working_dir(), expected);
}

#[tokio::test]
async fn multiple_cds_apply_in_order() {
    // The shell would execute them sequentially; the parser applies each
    // in source order, so the last successful one is the final state.
    let root = tempfile::tempdir().expect("tempdir");
    let a = root.path().join("a");
    let b = root.path().join("b");
    std::fs::create_dir(&a).expect("mkdir a");
    std::fs::create_dir(&b).expect("mkdir b");
    let a_canonical = a.canonicalize().expect("canonicalize a");
    let b_canonical = b.canonicalize().expect("canonicalize b");
    let ctx = ToolContext::empty();
    let tool = BashTool::new();
    let env = envelope(json!({
        "command": format!("cd {} && cd {}", a_canonical.display(), b_canonical.display()),
    }));
    tool.execute(&env, &ctx).await.expect("bash ok");
    assert_eq!(ctx.working_dir(), b_canonical);
}

#[test]
fn strip_surrounding_quotes_handles_double_quotes() {
    assert_eq!(strip_surrounding_quotes("\"foo bar\""), "foo bar");
}

#[test]
fn strip_surrounding_quotes_handles_single_quotes() {
    assert_eq!(strip_surrounding_quotes("'foo bar'"), "foo bar");
}

#[test]
fn strip_surrounding_quotes_leaves_unquoted_alone() {
    assert_eq!(strip_surrounding_quotes("foo"), "foo");
    assert_eq!(strip_surrounding_quotes("\"unmatched"), "\"unmatched");
    assert_eq!(strip_surrounding_quotes("'mismatched\""), "'mismatched\"");
}
