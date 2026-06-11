//! Integration tests for the `follow_up` tool's full execution path.
//!
//! These exercise the tool end-to-end through a real [`ToolRegistry`]: a
//! seeded [`ActionLog`] supplies the original call and its registered
//! follow-ups, a representative target tool is dispatched through the full
//! lifecycle, and the structured outputs are asserted. The action log and the
//! registry handle are published on the registry's shared [`ToolContext`] so
//! the tool resolves them exactly as it would at runtime.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;

use super::{FollowUpTool, SharedToolRegistry};
use crate::error::ToolError;
use crate::r#loop::runner::ToolExecutor;
use crate::session::action_log::{ActionLog, CompletionRecord, Outcome, hash_content};
use crate::session::store::EventStore;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::follow_up::{BeforeContentSource, Confidence, ExpiryCondition, FollowUpAction};
use crate::tool::lifecycle::{PostValidateMode, PostValidateOutcome};
use crate::tool::registry::ToolRegistry;
use crate::tool::scheduling::ToolEffect;
use crate::tool::traits::{Tool, ToolOutput};

/// Target tool that records the arguments it was dispatched with and echoes
/// them back. Optionally fails post-validate in `Gate` mode to prove the
/// target's full lifecycle runs.
struct RecordingTool {
    seen_args: Arc<Mutex<Option<serde_json::Value>>>,
    gate_fail: bool,
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &'static str {
        "recording_target"
    }

    fn description(&self) -> &'static str {
        "records dispatched args"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "additionalProperties": true })
    }

    fn effect(&self) -> ToolEffect {
        ToolEffect::ReadOnly
    }

    fn post_validate_mode(&self) -> PostValidateMode {
        if self.gate_fail {
            PostValidateMode::Gate
        } else {
            PostValidateMode::Report
        }
    }

    async fn execute(
        &self,
        envelope: &ToolEnvelope,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        *self.seen_args.lock() = Some(envelope.model_args.clone());
        Ok(ToolOutput::success(
            serde_json::json!({ "echo": envelope.model_args, "committed": true }),
        ))
    }

    async fn post_validate(&self, _output: &ToolOutput, _ctx: &ToolContext) -> PostValidateOutcome {
        if self.gate_fail {
            PostValidateOutcome::Fail {
                errors: vec!["target gate check failed".to_string()],
            }
        } else {
            PostValidateOutcome::Pass
        }
    }
}

/// Wired-up test fixture: a registry holding `follow_up` and a
/// [`RecordingTool`], a fresh action log, and a handle to the args the target
/// observed.
struct Harness {
    registry: Arc<ToolRegistry>,
    log: Arc<ActionLog>,
    seen_args: Arc<Mutex<Option<serde_json::Value>>>,
}

/// Build a registry containing `follow_up` and a [`RecordingTool`], wired with
/// a fresh action log and the registry self-handle on the shared context.
fn harness(gate_fail: bool) -> Harness {
    let seen_args = Arc::new(Mutex::new(None));
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(FollowUpTool::new()));
    registry.register(Box::new(RecordingTool {
        seen_args: Arc::clone(&seen_args),
        gate_fail,
    }));
    let registry = Arc::new(registry);

    let ctx = registry
        .shared_context()
        .expect("registry exposes a shared context");
    let log = Arc::new(ActionLog::new(Arc::new(EventStore::new())));
    ctx.insert_extension(Arc::clone(&log));
    ctx.insert_extension(Arc::new(SharedToolRegistry(Arc::clone(&registry))));

    Harness {
        registry,
        log,
        seen_args,
    }
}

fn follow_up_action(
    action: &str,
    tool: &str,
    args: serde_json::Value,
    expires: ExpiryCondition,
) -> FollowUpAction {
    FollowUpAction {
        action: action.to_owned(),
        description: format!("{action} via {tool}"),
        tool: tool.to_owned(),
        args,
        expires,
        confidence: Confidence::High,
        before_content: BeforeContentSource::Unavailable,
    }
}

/// Seed an original call `tool_call_id` with `args` and the given follow-ups.
fn seed_original(
    log: &ActionLog,
    tool_call_id: &str,
    args: serde_json::Value,
    follow_ups: Vec<FollowUpAction>,
) {
    log.record_completion(CompletionRecord {
        tool_name: "apply_patch",
        tool_call_id,
        tool_use_description: "original call",
        outcome: Outcome::Success,
        output: &serde_json::json!({}),
        args,
        duration_ms: 0,
        follow_ups,
        post_validate_outcome: None,
        level_1_only: false,
    });
}

#[tokio::test]
async fn follow_up_merges_override_and_dispatches_target() {
    let Harness {
        registry,
        log,
        seen_args,
    } = harness(false);
    seed_original(
        &log,
        "tc-orig",
        serde_json::json!({ "path": "src/a.rs", "mode": "auto", "patch": "BIG PATCH TEXT" }),
        vec![follow_up_action(
            "apply_structural",
            "recording_target",
            serde_json::json!({ "mode": "structural" }),
            ExpiryCondition::Never,
        )],
    );

    let output = registry
        .execute(
            "follow_up",
            "test-call",
            serde_json::json!({ "tool_call_id": "tc-orig", "action": "apply_structural" }),
        )
        .await
        .expect("dispatch succeeds");
    let content = output;

    // The target saw the merged args: override mode replaced the original,
    // the large patch text was inherited from the original call.
    let seen = seen_args.lock().clone().expect("target was dispatched");
    assert_eq!(seen["mode"], "structural");
    assert_eq!(seen["path"], "src/a.rs");
    assert_eq!(seen["patch"], "BIG PATCH TEXT");

    // The result is the target's output, verbatim.
    assert_eq!(content["echo"]["mode"], "structural");
    assert_eq!(content["committed"], true);
}

#[tokio::test]
async fn expired_file_modified_follow_up_returns_error() {
    let Harness { registry, log, .. } = harness(false);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("handler.rs");
    std::fs::write(&path, b"current contents, different from registration").unwrap();

    seed_original(
        &log,
        "tc-orig",
        serde_json::json!({ "path": "handler.rs" }),
        vec![follow_up_action(
            "reapply",
            "recording_target",
            serde_json::json!({}),
            ExpiryCondition::FileModified {
                path: path.clone(),
                content_hash: hash_content(b"the contents at registration time"),
            },
        )],
    );

    let output = registry
        .execute(
            "follow_up",
            "test-call",
            serde_json::json!({ "tool_call_id": "tc-orig", "action": "reapply" }),
        )
        .await
        .expect("dispatch returns structured error");
    let content = output;

    assert_eq!(content["error"]["kind"], "blocked");
    assert!(
        content["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&path.display().to_string()),
        "message should name the changed file: {content}",
    );
    // The pre-payload expiry fields stay model-visible alongside the error.
    assert_eq!(content["action"], "reapply");
}

#[tokio::test]
async fn nonexistent_tool_call_id_returns_error() {
    let Harness { registry, .. } = harness(false);

    let output = registry
        .execute(
            "follow_up",
            "test-call",
            serde_json::json!({ "tool_call_id": "does-not-exist", "action": "undo" }),
        )
        .await
        .expect("dispatch returns structured error");
    let content = output;

    assert_eq!(content["error"]["kind"], "not_found");
    assert_eq!(content["error"]["message"], "tool_call_id not found");
    assert_eq!(content["tool_call_id"], "does-not-exist");
}

#[tokio::test]
async fn nonexistent_action_returns_error_with_available_actions() {
    let Harness { registry, log, .. } = harness(false);

    // One valid (Never) action and one expired (unreadable file) action.
    seed_original(
        &log,
        "tc-orig",
        serde_json::json!({}),
        vec![
            follow_up_action(
                "undo",
                "recording_target",
                serde_json::json!({}),
                ExpiryCondition::Never,
            ),
            follow_up_action(
                "reapply",
                "recording_target",
                serde_json::json!({}),
                ExpiryCondition::FileModified {
                    path: std::path::PathBuf::from("/definitely/not/here.rs"),
                    content_hash: "deadbeef".to_owned(),
                },
            ),
        ],
    );

    let output = registry
        .execute(
            "follow_up",
            "test-call",
            serde_json::json!({ "tool_call_id": "tc-orig", "action": "ghost" }),
        )
        .await
        .expect("dispatch returns structured error");
    let content = output;

    assert_eq!(content["error"]["kind"], "not_found");
    assert_eq!(content["error"]["message"], "action not found");
    assert_eq!(content["tool_call_id"], "tc-orig");
    assert_eq!(content["action"], "ghost");
    let available = content["available_actions"].as_array().unwrap();
    let names: Vec<&str> = available.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(
        names,
        vec!["undo"],
        "expired actions must be excluded from available_actions",
    );
}

#[tokio::test]
async fn target_gate_mode_post_validate_failure_propagates() {
    let Harness { registry, log, .. } = harness(true);
    seed_original(
        &log,
        "tc-orig",
        serde_json::json!({ "path": "src/a.rs" }),
        vec![follow_up_action(
            "reapply",
            "recording_target",
            serde_json::json!({}),
            ExpiryCondition::Never,
        )],
    );

    let err = registry
        .execute(
            "follow_up",
            "test-call",
            serde_json::json!({ "tool_call_id": "tc-orig", "action": "reapply" }),
        )
        .await
        .expect_err("target gate failure surfaces as an error");

    match err {
        ToolError::PostValidationFailed { reason, .. } => {
            assert!(
                reason.contains("target gate check failed"),
                "gate failure reason must surface: {reason}",
            );
        }
        other => panic!("expected PostValidationFailed, got {other:?}"),
    }
}

#[tokio::test]
async fn follow_up_result_has_no_follow_ups_key() {
    let Harness { registry, log, .. } = harness(false);
    seed_original(
        &log,
        "tc-orig",
        serde_json::json!({ "path": "src/a.rs" }),
        vec![follow_up_action(
            "reapply",
            "recording_target",
            serde_json::json!({}),
            ExpiryCondition::Never,
        )],
    );

    let output = registry
        .execute(
            "follow_up",
            "test-call",
            serde_json::json!({ "tool_call_id": "tc-orig", "action": "reapply" }),
        )
        .await
        .expect("dispatch succeeds");
    let content = output;

    assert!(
        content.get("follow_ups").is_none(),
        "follow_up's own result must not carry a follow_ups key: {content}",
    );
}

// NOTE: `source_tool_call_id` chain-tracking test removed — the field is
// not present on `ActionLogEntry` in main. Will be re-added when the
// follow-up chaining infrastructure lands.
