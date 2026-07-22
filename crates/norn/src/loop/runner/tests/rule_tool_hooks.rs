use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Register a `**/*.rs` path-glob rule with `SystemContextAppend`
/// delivery, then run a turn that calls the `write` tool on a `.rs`
/// file. The operator rule body must appear once as a Developer conversation
/// message on the next request while the System message stays stable.
#[tokio::test]
async fn rule_with_path_glob_fires_when_write_tool_runs() -> TestResult {
    use std::sync::Arc;

    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, PreLlmHook};
    use crate::provider::request::{MessageRole, ProviderRequest};
    use crate::rules::engine::RuleEngine;
    use crate::rules::types::{
        DeliveryMode as RDM, Rule, RuleId, TriggerCondition, TriggerTiming as TT,
    };

    #[derive(Clone)]
    struct CapturedTurn {
        system: String,
        developer_messages: Vec<String>,
    }
    struct CaptureSystem {
        captured: Arc<parking_lot::Mutex<Vec<CapturedTurn>>>,
    }
    #[async_trait::async_trait]
    impl PreLlmHook for CaptureSystem {
        async fn before_llm(&self, req: &ProviderRequest) -> HookOutcome {
            let system = req
                .messages
                .first()
                .and_then(|m| m.content.clone())
                .unwrap_or_default();
            let developer_messages = req
                .messages
                .iter()
                .filter(|message| matches!(message.role, MessageRole::Developer))
                .filter_map(|message| message.content.clone())
                .collect();
            self.captured.lock().push(CapturedTurn {
                system,
                developer_messages,
            });
            HookOutcome::Proceed
        }
    }

    let turn1 = vec![
        tool_call_delta("tc_write", Some("write"), r#"{"path":"src/lib.rs"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"answer":"ok"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "write".to_string(),
        Box::new(|_| Ok(serde_json::json!({"status": "written"}))),
    );
    let executor = MockToolExecutor::new(handlers);
    let schema = simple_schema();
    let write_tool = ToolDefinition {
        name: "write".to_string(),
        description: "Write a file".to_string(),
        parameters: serde_json::json!({}),
    };
    let rule = Rule {
        id: RuleId::from("rust-conventions"),
        name: "Rust Conventions".to_string(),
        triggers: vec![TriggerCondition::PathGlob {
            pattern: "**/*.rs".to_string(),
        }],
        delivery: RDM::SystemContextAppend,
        timing: TT::Before,
        body: "Follow Rust conventions.".to_string(),
        shell_source: None,
    };

    let captured: Arc<parking_lot::Mutex<Vec<CapturedTurn>>> =
        Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreLlm(Box::new(CaptureSystem {
        captured: Arc::clone(&captured),
    })));

    let mut loop_ctx = LoopContext::new("base-system");
    loop_ctx.rules = Some(RuleEngine::new(vec![rule]));
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));
    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[write_tool],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "ok");

    let snapshots = captured.lock().clone();
    assert_eq!(snapshots.len(), 2, "expected two provider calls");
    assert_eq!(snapshots[0].system, "base-system");
    assert!(
        snapshots[0]
            .developer_messages
            .iter()
            .all(|message| !message.contains("Follow Rust conventions."))
    );
    assert_eq!(snapshots[1].system, "base-system");
    let matching_rules = snapshots[1]
        .developer_messages
        .iter()
        .filter(|message| message.contains("Follow Rust conventions."))
        .count();
    assert_eq!(
        matching_rules, 1,
        "operator rule must appear in exactly one Developer message: {:?}",
        snapshots[1].developer_messages,
    );
    Ok(())
}

/// A `PreToolHook` that blocks bash must persist the block result and
/// prevent the executor from running.
#[tokio::test]
async fn pre_tool_hook_blocks_bash() -> TestResult {
    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, PreToolHook};
    use crate::tool::context::ToolContext;
    use crate::tool::envelope::ToolEnvelope;

    struct BlockBash;
    #[async_trait::async_trait]
    impl PreToolHook for BlockBash {
        async fn before_tool(&self, envelope: &ToolEnvelope, _ctx: &ToolContext) -> HookOutcome {
            if envelope.tool_name == "bash" {
                HookOutcome::Block {
                    reason: "bash blocked".to_owned(),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    let turn1 = vec![
        tool_call_delta("tc_bash", Some("bash"), r#"{"command":"ls"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"after-block"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "bash".to_string(),
        Box::new(|_| {
            Err(crate::error::ToolError::ExecutionFailed {
                reason: "bash executor ran despite the pre-tool block".to_owned(),
            })
        }),
    );
    let executor = MockToolExecutor::new(handlers);
    let schema = simple_schema();
    let bash_tool = ToolDefinition {
        name: "bash".to_string(),
        description: "Run bash".to_string(),
        parameters: serde_json::json!({}),
    };

    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreTool(Box::new(BlockBash)));
    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));
    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[bash_tool],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "after-block");
    let events = store.events();
    let bash_output = events
        .iter()
        .find_map(|event| match event {
            SessionEvent::ToolResult {
                tool_name, output, ..
            } if tool_name == "bash" => Some(output.clone()),
            _ => None,
        })
        .ok_or_else(|| std::io::Error::other("bash ToolResult is missing"))?;
    assert_eq!(bash_output["error"]["kind"], "blocked");
    let message = bash_output["error"]["message"].as_str().unwrap_or("");
    assert!(
        message.contains("blocked by hook") && message.contains("bash blocked"),
        "expected block reason in bash output, got: {bash_output}",
    );
    Ok(())
}

/// A `PreToolHook` can replace the arguments passed to the executor.
#[tokio::test]
async fn pre_tool_hook_modifies_bash_args() -> TestResult {
    use std::sync::{Arc, Mutex};

    use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, PreToolHook};
    use crate::tool::context::ToolContext;
    use crate::tool::envelope::ToolEnvelope;

    struct RewriteBash;
    #[async_trait::async_trait]
    impl PreToolHook for RewriteBash {
        async fn before_tool(&self, envelope: &ToolEnvelope, _ctx: &ToolContext) -> HookOutcome {
            if envelope.tool_name == "bash" {
                HookOutcome::Modify {
                    updated_input: serde_json::json!({ "command": "echo modified" }),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    let turn1 = vec![
        tool_call_delta("tc_bash", Some("bash"), r#"{"command":"ls"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"after-modify"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    let recorded: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let recorded_for_handler = Arc::clone(&recorded);
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "bash".to_string(),
        Box::new(move |args| {
            let mut slot = recorded_for_handler
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *slot = Some(args);
            Ok(serde_json::json!({"stdout": "modified"}))
        }),
    );
    let executor = MockToolExecutor::new(handlers);
    let schema = simple_schema();
    let bash_tool = ToolDefinition {
        name: "bash".to_string(),
        description: "Run bash".to_string(),
        parameters: serde_json::json!({}),
    };

    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreTool(Box::new(RewriteBash)));
    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));
    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[bash_tool],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "after-modify");
    let seen = recorded
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
        .ok_or_else(|| std::io::Error::other("bash handler was not invoked"))?;
    assert_eq!(seen["command"], "echo modified");
    Ok(())
}
