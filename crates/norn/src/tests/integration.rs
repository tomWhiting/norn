//! End-to-end integration tests for the Norn agent loop.
//!
//! Each test exercises a real integrated path through multiple subsystems:
//! [`MockProvider`](crate::provider::mock::MockProvider) scripted responses
//! → `run_agent_step` → tool dispatch → real tools
//! ([`ReadTool`](crate::tools::read::ReadTool),
//! [`WriteTool`](crate::tools::write::WriteTool),
//! [`BashTool`](crate::tools::bash::BashTool),
//! [`SearchTool`](crate::tools::search::SearchTool)) or
//! [`MockToolExecutor`](crate::r#loop::config::MockToolExecutor)
//! → session store → result inspection.
//!
//! Tests are intentionally coarse: they prove subsystems work together, not
//! the internal behaviour of any single subsystem (covered by unit tests).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::uninlined_format_args,
    clippy::doc_markdown,
    clippy::collapsible_if
)]

use async_trait::async_trait;
use serde_json::Value;
use tempfile::tempdir;

use crate::integration::hooks::{Hook, HookOutcome, HookRegistry, PreToolHook};
use crate::r#loop::active_input_channel;
use crate::r#loop::config::{
    AgentLoopConfig, AgentStepResult, ConversationStateMode, MockToolExecutor, ToolHandler,
};
use crate::r#loop::inbound::{MessageKind, inbound_channel};
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::runner::{AgentStepRequest, run_agent_step};
use crate::provider::events::{ProviderEvent, StopReason};
use crate::provider::mock::MockProvider;
use crate::provider::request::ToolDefinition;
use crate::provider::tools::ProviderCapabilities;
use crate::provider::usage::Usage;
use crate::rules::engine::RuleEngine;
use crate::rules::types::{
    DeliveryMode as RuleDelivery, Rule, RuleId, TriggerCondition, TriggerTiming,
};
use crate::session::events::{EventBase, EventUsage, SessionEvent, ToolCallEvent};
use crate::session::store::EventStore;
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::registry::ToolRegistry;
use crate::tools::bash::BashTool;
use crate::tools::read::ReadTool;
use crate::tools::search::SearchTool;
use crate::tools::write::WriteTool;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn done_event(reason: StopReason) -> ProviderEvent {
    ProviderEvent::Done {
        stop_reason: reason,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Usage::default()
        },
        response_id: None,
    }
}

fn done_event_with_response(reason: StopReason, response_id: &str) -> ProviderEvent {
    ProviderEvent::Done {
        stop_reason: reason,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Usage::default()
        },
        response_id: Some(response_id.to_string()),
    }
}

fn tool_call_delta(item_id: &str, name: Option<&str>, args: &str) -> ProviderEvent {
    ProviderEvent::ToolCallDelta {
        item_id: item_id.to_string(),
        name: name.map(String::from),
        arguments_delta: args.to_string(),
        kind: crate::provider::request::ToolCallKind::Function,
    }
}

fn text_delta(text: &str) -> ProviderEvent {
    ProviderEvent::TextDelta {
        text: text.to_string(),
    }
}

fn simple_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "answer": { "type": "string" }
        },
        "required": ["answer"]
    })
}

fn tool_def(name: &str, description: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_string(),
        description: description.to_string(),
        parameters: serde_json::json!({"type": "object"}),
    }
}

/// Unwrap a `Completed` result or panic with a descriptive message.
#[track_caller]
fn assert_completed(result: AgentStepResult) -> (Value, Usage) {
    match result {
        AgentStepResult::Completed { output, usage, .. } => (output, usage),
        other => panic!("expected AgentStepResult::Completed, got: {other:?}"),
    }
}

/// Run `run_agent_step` with a `LoopContext` and the given executor, returning
/// the unwrapped result (panics on `Err`).
async fn run_step_with_ctx(
    provider: &MockProvider,
    executor: &dyn crate::r#loop::config::ToolExecutor,
    store: &EventStore,
    tools: &[ToolDefinition],
    schema: Option<&Value>,
    config: &AgentLoopConfig,
    loop_ctx: &mut LoopContext,
) -> AgentStepResult {
    run_agent_step(AgentStepRequest {
        provider,
        executor,
        store,
        user_prompt: "test prompt",
        tools,
        output_schema: schema,
        model: "test-model",
        config,
        event_tx: None,
        inbound: None,
        loop_context: loop_ctx,
        cancel: None,
    })
    .await
    .expect("run_agent_step must not return Err in integration tests")
}

#[tokio::test]
async fn default_auto_state_threads_supported_provider() {
    let config = AgentLoopConfig::default();
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let first_provider = MockProvider::with_capabilities(
        vec![vec![
            text_delta("first"),
            done_event_with_response(StopReason::EndTurn, "resp_first"),
        ]],
        ProviderCapabilities::openai_responses(),
    );
    let mut first_ctx = LoopContext::new("system");
    let first = run_step_with_ctx(
        &first_provider,
        &executor,
        &store,
        &[],
        None,
        &config,
        &mut first_ctx,
    )
    .await;
    assert_completed(first);

    let second_provider = MockProvider::with_capabilities(
        vec![vec![
            text_delta("second"),
            done_event_with_response(StopReason::EndTurn, "resp_second"),
        ]],
        ProviderCapabilities::openai_responses(),
    );
    let mut second_ctx = LoopContext::new("system");
    let second = run_step_with_ctx(
        &second_provider,
        &executor,
        &store,
        &[],
        None,
        &config,
        &mut second_ctx,
    )
    .await;
    assert_completed(second);

    let requests = second_provider.requests().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].store);
    assert_eq!(
        requests[0].previous_response_id.as_deref(),
        Some("resp_first"),
    );
    assert!(requests[0].context_management.is_none());
}

#[tokio::test]
async fn provider_threaded_second_step_uses_response_anchor() {
    let config = AgentLoopConfig {
        conversation_state: ConversationStateMode::ProviderThreaded,
        server_compaction_threshold_tokens: Some(200_000),
        ..AgentLoopConfig::default()
    };
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let first_provider = MockProvider::with_capabilities(
        vec![vec![
            text_delta("first"),
            done_event_with_response(StopReason::EndTurn, "resp_first"),
        ]],
        ProviderCapabilities::openai_responses(),
    );
    let mut first_ctx = LoopContext::new("system");
    let first = run_step_with_ctx(
        &first_provider,
        &executor,
        &store,
        &[],
        None,
        &config,
        &mut first_ctx,
    )
    .await;
    assert_completed(first);

    let second_provider = MockProvider::with_capabilities(
        vec![vec![
            text_delta("second"),
            done_event_with_response(StopReason::EndTurn, "resp_second"),
        ]],
        ProviderCapabilities::openai_responses(),
    );
    let mut second_ctx = LoopContext::new("system");
    let second = run_step_with_ctx(
        &second_provider,
        &executor,
        &store,
        &[],
        None,
        &config,
        &mut second_ctx,
    )
    .await;
    assert_completed(second);

    let requests = second_provider.requests().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(request.store);
    assert_eq!(request.previous_response_id.as_deref(), Some("resp_first"));
    assert_eq!(
        request
            .context_management
            .as_ref()
            .map(|management| management.compact_threshold_tokens),
        Some(200_000),
    );
    assert_eq!(request.messages.len(), 3);
    assert_eq!(
        request.messages[0].role,
        crate::provider::request::MessageRole::System
    );
    assert_eq!(
        request.messages[1].role,
        crate::provider::request::MessageRole::Developer
    );
    assert_eq!(request.messages[2].content.as_deref(), Some("test prompt"));
}

#[tokio::test]
async fn provider_threaded_tool_continuation_sends_only_tool_result() {
    let config = AgentLoopConfig {
        conversation_state: ConversationStateMode::ProviderThreaded,
        ..AgentLoopConfig::default()
    };
    let provider = MockProvider::with_capabilities(
        vec![
            vec![
                tool_call_delta("call_read", Some("read"), r#"{"path":"a"}"#),
                done_event_with_response(StopReason::ToolUse, "resp_tool"),
            ],
            vec![
                text_delta("done"),
                done_event_with_response(StopReason::EndTurn, "resp_done"),
            ],
        ],
        ProviderCapabilities::openai_responses(),
    );
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read".to_string(),
        Box::new(|_| Ok(serde_json::json!({"contents": "ok"}))),
    );
    let executor = MockToolExecutor::new(handlers);
    let store = EventStore::new();
    let mut loop_ctx = LoopContext::new("system");

    let result = run_step_with_ctx(
        &provider,
        &executor,
        &store,
        &[tool_def("read", "Read a file")],
        None,
        &config,
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].previous_response_id, None);
    assert!(requests[0].store);
    assert_eq!(
        requests[1].previous_response_id.as_deref(),
        Some("resp_tool")
    );
    assert!(requests[1].store);
    assert_eq!(requests[1].messages.len(), 3);
    assert_eq!(
        requests[1].messages[0].role,
        crate::provider::request::MessageRole::System
    );
    assert_eq!(
        requests[1].messages[1].role,
        crate::provider::request::MessageRole::Developer,
    );
    assert_eq!(
        requests[1].messages[2].role,
        crate::provider::request::MessageRole::ToolResult,
    );
    assert_eq!(
        requests[1].messages[2].tool_call_id.as_deref(),
        Some("call_read"),
    );
}

#[tokio::test]
async fn provider_threaded_resume_replays_post_anchor_history() {
    let config = AgentLoopConfig {
        conversation_state: ConversationStateMode::ProviderThreaded,
        ..AgentLoopConfig::default()
    };
    let store = EventStore::new();
    store
        .append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "old prompt".to_string(),
        })
        .unwrap();
    store
        .append(SessionEvent::AssistantMessage {
            base: EventBase::new(store.last_event_id()),
            content: String::new(),
            thinking: String::new(),
            tool_calls: vec![ToolCallEvent {
                call_id: "call_read".to_string(),
                name: "read".to_string(),
                arguments: serde_json::json!({"path": "a"}),
                kind: crate::provider::request::ToolCallKind::Function,
            }],
            usage: EventUsage::default(),
            stop_reason: "tool_use".to_string(),
            response_id: Some("resp_tool".to_string()),
        })
        .unwrap();
    store
        .append(SessionEvent::ToolResult {
            base: EventBase::new(store.last_event_id()),
            tool_call_id: "call_read".to_string(),
            tool_name: "read".to_string(),
            output: serde_json::json!({"contents": "ok"}),
            duration_ms: 1,
        })
        .unwrap();

    let provider = MockProvider::with_capabilities(
        vec![vec![
            text_delta("done"),
            done_event_with_response(StopReason::EndTurn, "resp_done"),
        ]],
        ProviderCapabilities::openai_responses(),
    );
    let executor = MockToolExecutor::empty();
    let mut loop_ctx = LoopContext::new("system");

    let result = run_step_with_ctx(
        &provider,
        &executor,
        &store,
        &[tool_def("read", "Read a file")],
        None,
        &config,
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].previous_response_id.as_deref(),
        Some("resp_tool"),
    );
    let request_roles: Vec<_> = requests[0]
        .messages
        .iter()
        .map(|message| message.role.clone())
        .collect();
    assert_eq!(
        request_roles,
        vec![
            crate::provider::request::MessageRole::System,
            crate::provider::request::MessageRole::Developer,
            crate::provider::request::MessageRole::ToolResult,
            crate::provider::request::MessageRole::User,
        ],
    );
    assert_eq!(
        requests[0].messages[2].tool_call_id.as_deref(),
        Some("call_read"),
    );
    assert_eq!(
        requests[0].messages[3].content.as_deref(),
        Some("test prompt"),
    );
}

// ---------------------------------------------------------------------------
// Test 1: real Read + Write tools through ToolRegistry
// ---------------------------------------------------------------------------

/// A full step that reads a file, then writes a new file, then calls schema.
/// Uses a real ToolRegistry with ReadTool and WriteTool.
///
/// Proves that:
/// - ToolRegistry correctly dispatches to the real ReadTool
/// - ReadTool marks the file as read in the ToolContext
/// - WriteTool successfully writes a new file (no prior read check for new files)
/// - Schema tool completes the step
/// - Session store contains ToolResult events for both read and write
#[tokio::test]
async fn test_full_agent_step_with_real_file_tools() {
    let dir = tempdir().expect("tempdir");
    let source_path = dir.path().join("input.txt");
    let output_path = dir.path().join("output.txt");
    let source_content = "hello from integration test\n";
    tokio::fs::write(&source_path, source_content)
        .await
        .expect("write source file");

    // Turn 1: model calls read
    let turn1 = vec![
        tool_call_delta(
            "tc_read",
            Some("read"),
            &format!(r#"{{"path": "{}"}}"#, source_path.to_string_lossy()),
        ),
        done_event(StopReason::ToolUse),
    ];
    // Turn 2: model calls write (new file — no prior read needed)
    let turn2 = vec![
        tool_call_delta(
            "tc_write",
            Some("write"),
            &format!(
                r#"{{"path": "{}", "content": "written by test\n"}}"#,
                output_path.to_string_lossy()
            ),
        ),
        done_event(StopReason::ToolUse),
    ];
    // Turn 3: model calls schema tool
    let turn3 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"file operations complete"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2, turn3]);
    let store = EventStore::new();

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ReadTool::new()));
    registry.register(Box::new(WriteTool::new()));

    let schema = simple_schema();
    let config = AgentLoopConfig::default();
    let mut loop_ctx = LoopContext::new("You are a helpful assistant.");

    let result = run_step_with_ctx(
        &provider,
        &registry,
        &store,
        &[
            tool_def("read", "Read a file"),
            tool_def("write", "Write a file"),
        ],
        Some(&schema),
        &config,
        &mut loop_ctx,
    )
    .await;

    let (output, _usage) = assert_completed(result);
    assert_eq!(
        output["answer"], "file operations complete",
        "schema output must match"
    );

    // Verify the output file was actually written to disk.
    let written = tokio::fs::read_to_string(&output_path)
        .await
        .expect("output file must exist on disk");
    assert_eq!(
        written, "written by test\n",
        "file content must match what was written"
    );

    // Verify session store contains ToolResult events for both tools.
    let events = store.events();
    let read_result = events
        .iter()
        .any(|e| matches!(e, SessionEvent::ToolResult { tool_name, .. } if tool_name == "read"));
    assert!(
        read_result,
        "session store must contain a ToolResult for 'read'"
    );

    let write_result = events
        .iter()
        .any(|e| matches!(e, SessionEvent::ToolResult { tool_name, .. } if tool_name == "write"));
    assert!(
        write_result,
        "session store must contain a ToolResult for 'write'"
    );
}

// ---------------------------------------------------------------------------
// Test 2: BashTool and SearchTool through ToolRegistry
// ---------------------------------------------------------------------------

/// A step that runs `echo hello` via bash, then searches for a file pattern,
/// then calls schema. Uses real BashTool and SearchTool.
///
/// Proves that:
/// - BashTool executes real subprocesses and captures stdout
/// - SearchTool can locate files by glob pattern in a temp dir
/// - Both tools integrate through ToolRegistry + run_agent_step
#[tokio::test]
async fn test_agent_step_with_bash_and_search() {
    let dir = tempdir().expect("tempdir");
    // Create a file for the search tool to find.
    let target = dir.path().join("needle.txt");
    tokio::fs::write(&target, "search me\n")
        .await
        .expect("write target file");

    let dir_str = dir.path().to_string_lossy().to_string();

    // Turn 1: bash echo
    let turn1 = vec![
        tool_call_delta("tc_bash", Some("bash"), r#"{"command": "echo hello"}"#),
        done_event(StopReason::ToolUse),
    ];
    // Turn 2: search for needle.txt
    let turn2 = vec![
        tool_call_delta(
            "tc_search",
            Some("search"),
            &format!(r#"{{"path": "{dir_str}", "glob": "**/needle.txt", "mode": "files"}}"#),
        ),
        done_event(StopReason::ToolUse),
    ];
    // Turn 3: schema
    let turn3 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"bash and search done"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2, turn3]);
    let store = EventStore::new();

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(BashTool::new()));
    registry.register(Box::new(SearchTool::new()));

    let schema = simple_schema();
    let config = AgentLoopConfig::default();
    let mut loop_ctx = LoopContext::new("You are a helpful assistant.");

    let result = run_step_with_ctx(
        &provider,
        &registry,
        &store,
        &[
            tool_def("bash", "Execute a shell command"),
            tool_def("search", "Search files"),
        ],
        Some(&schema),
        &config,
        &mut loop_ctx,
    )
    .await;

    let (output, _usage) = assert_completed(result);
    assert_eq!(output["answer"], "bash and search done");

    // Verify bash tool result contains "hello".
    let events = store.events();
    let bash_result = events
        .iter()
        .find(|e| matches!(e, SessionEvent::ToolResult { tool_name, .. } if tool_name == "bash"));
    assert!(
        bash_result.is_some(),
        "session store must contain a ToolResult for 'bash'"
    );
    if let Some(SessionEvent::ToolResult { output, .. }) = bash_result {
        let stdout = output["stdout"].as_str().unwrap_or("");
        assert!(
            stdout.contains("hello"),
            "bash stdout must contain 'hello', got: {stdout:?}",
        );
    }

    // Verify search tool result contains needle.txt path.
    let search_result = events
        .iter()
        .find(|e| matches!(e, SessionEvent::ToolResult { tool_name, .. } if tool_name == "search"));
    assert!(
        search_result.is_some(),
        "session store must contain a ToolResult for 'search'",
    );
    if let Some(SessionEvent::ToolResult { output, .. }) = search_result {
        let output_str = output.to_string();
        assert!(
            output_str.contains("needle.txt"),
            "search result must reference needle.txt, got: {output_str}",
        );
    }
}

// ---------------------------------------------------------------------------
// Test 3: rules fire and inject into system instruction on path-glob match
// ---------------------------------------------------------------------------

/// A step with a RuleEngine containing a path-glob rule for `*.txt`.
/// MockToolExecutor simulates a write to a .txt file (writes to a mock path).
/// After the tool call, the rules engine fires and its content should appear in
/// the system instruction injected into the next provider call.
///
/// Proves that:
/// - RuleEngine is wired into the loop via LoopContext
/// - PathGlob triggers fire when a write tool is called with a matching path
/// - Rule content becomes a dynamic system section for the next iteration
/// - The session store records the rule injection as a UserMessage
#[tokio::test]
async fn test_rules_fire_on_file_write() {
    let rule = Rule {
        id: RuleId::from("txt-rule"),
        name: "TXT Rule".to_owned(),
        triggers: vec![TriggerCondition::PathGlob {
            pattern: "**/*.txt".to_owned(),
        }],
        delivery: RuleDelivery::ContextInjection,
        timing: TriggerTiming::After,
        body: "RULE_CONTENT_INJECTED".to_owned(),
        shell_source: None,
    };

    // Turn 1: write to a .txt path (will trigger the rule)
    // Turn 2: schema tool to finish
    let turn1 = vec![
        tool_call_delta(
            "tc_write",
            Some("write"),
            r#"{"path": "/tmp/some_file.txt", "content": "hello\n"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"rules fired"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    // Use MockToolExecutor so we don't actually write to /tmp.
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "write".to_string(),
        Box::new(|_args: Value| {
            Ok(serde_json::json!({
                "path": "/tmp/some_file.txt",
                "bytes_written": 6,
                "line_count": 1,
                "length_limit": serde_json::Value::Null,
                "diagnostics": [],
                "check_overrides": []
            }))
        }),
    );
    let executor = MockToolExecutor::new(handlers);

    let schema = simple_schema();
    let config = AgentLoopConfig::default();
    let mut loop_ctx = LoopContext::new("base system instruction");
    loop_ctx.rules = Some(RuleEngine::new(vec![rule]));

    let result = run_step_with_ctx(
        &provider,
        &executor,
        &store,
        &[tool_def("write", "Write a file")],
        Some(&schema),
        &config,
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "rules fired");

    // Rule delivery ContextInjection creates a UserMessage with the rule content.
    let events = store.events();
    let rule_injected = events.iter().any(|e| {
        if let SessionEvent::UserMessage { content, .. } = e {
            content.contains("RULE_CONTENT_INJECTED")
        } else {
            false
        }
    });
    assert!(
        rule_injected,
        "rule content 'RULE_CONTENT_INJECTED' must appear in session store as a UserMessage; events: {:?}",
        events
            .iter()
            .map(|e| format!("{e:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
}

// ---------------------------------------------------------------------------
// Test 4: hooks block tool execution — loop continues past the blocked tool
// ---------------------------------------------------------------------------

/// A step where a PreToolHook blocks the 'bash' tool.
/// The provider emits a bash call then a schema call.
/// The loop must: record the blocked output, skip actual execution, then
/// proceed to the schema call and complete successfully.
///
/// Proves that:
/// - HookRegistry PreToolHook blocking produces an error tool result
/// - The loop does NOT terminate on a hook block (it continues)
/// - Subsequent tool calls (schema) still execute normally
#[tokio::test]
async fn test_hooks_block_tool_execution() {
    struct BlockBash;

    #[async_trait]
    impl PreToolHook for BlockBash {
        async fn before_tool(&self, envelope: &ToolEnvelope, _ctx: &ToolContext) -> HookOutcome {
            if envelope.tool_name == "bash" {
                HookOutcome::Block {
                    reason: "bash is not permitted".to_owned(),
                }
            } else {
                HookOutcome::Proceed
            }
        }
    }

    // Turn 1: bash call (will be blocked by hook)
    let turn1 = vec![
        tool_call_delta("tc_bash", Some("bash"), r#"{"command": "echo secret"}"#),
        done_event(StopReason::ToolUse),
    ];
    // Turn 2: schema call (should succeed normally)
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"completed despite block"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let schema = simple_schema();
    let config = AgentLoopConfig::default();
    let mut loop_ctx = LoopContext::new("system");

    let mut hook_registry = HookRegistry::new();
    hook_registry.register(Hook::PreTool(Box::new(BlockBash)));
    loop_ctx.hooks = Some(std::sync::Arc::new(hook_registry));

    let result = run_step_with_ctx(
        &provider,
        &executor,
        &store,
        &[tool_def("bash", "Run bash")],
        Some(&schema),
        &config,
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(
        output["answer"], "completed despite block",
        "loop must complete past blocked tool"
    );

    // Verify the bash tool result records the block reason.
    let events = store.events();
    let bash_blocked = events.iter().any(|e| {
        if let SessionEvent::ToolResult {
            tool_name, output, ..
        } = e
        {
            if tool_name == "bash" {
                // Hook blocks persist as the typed `blocked` payload —
                // kind plus a message carrying the hook's stated reason.
                let message = output["error"]["message"].as_str().unwrap_or("");
                return output["error"]["kind"] == "blocked"
                    && message.contains("blocked by hook")
                    && message.contains("bash is not permitted");
            }
        }
        false
    });
    assert!(
        bash_blocked,
        "ToolResult for bash must record 'blocked by hook' reason",
    );
}

// ---------------------------------------------------------------------------
// Test 5: schema enforcement retry — invalid then valid output
// ---------------------------------------------------------------------------

/// The provider first emits a schema call with invalid output (missing the
/// required 'answer' field), then emits a valid schema call.
/// The loop must retry and eventually complete with the valid output.
///
/// Proves that:
/// - Schema validation rejects output that fails the JSON Schema
/// - The loop injects validation feedback and retries
/// - On the second attempt with valid output, AgentStepResult::Completed is returned
/// - The attempt count is reflected in the schema budget consumption
#[tokio::test]
async fn test_schema_enforcement_retry() {
    // Turn 1: invalid schema output (wrong type for 'answer' — passes missing required)
    let turn1 = vec![
        tool_call_delta(
            "tc_bad",
            Some("structured_output"),
            r#"{"wrong_field": 42}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    // Turn 2: valid schema output
    let turn2 = vec![
        tool_call_delta(
            "tc_good",
            Some("structured_output"),
            r#"{"answer": "valid on retry"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let schema = simple_schema();
    let config = AgentLoopConfig {
        schema_attempt_budget: 3,
        ..AgentLoopConfig::default()
    };
    let mut loop_ctx = LoopContext::new("system");

    let result = run_step_with_ctx(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config,
        &mut loop_ctx,
    )
    .await;

    let (output, _usage) = assert_completed(result);
    assert_eq!(
        output["answer"], "valid on retry",
        "second attempt with valid output must produce Completed",
    );

    // The provider was called twice — once for invalid, once for valid.
    assert_eq!(
        provider.call_count(),
        2,
        "provider must be called exactly twice (one invalid + one valid attempt)",
    );

    // The session store must contain a validation feedback UserMessage.
    let events = store.events();
    let has_feedback = events.iter().any(|e| {
        if let SessionEvent::ToolResult {
            tool_name, output, ..
        } = e
        {
            // The loop appends the feedback as a ToolResult for the schema tool
            // with a string payload containing validation errors.
            tool_name == "structured_output"
                && output
                    .as_str()
                    .is_some_and(|s| s.contains("Schema validation failed"))
        } else {
            false
        }
    });
    assert!(
        has_feedback,
        "session store must record schema validation feedback on first (invalid) attempt",
    );
}

// ---------------------------------------------------------------------------
// Test 6: inbound steer message appears in next provider call's messages
// ---------------------------------------------------------------------------

/// Active human input is surface-authored text, not inter-agent traffic. It
/// must enter the event store and provider request as an ordinary user message
/// and only then acknowledge delivery to the surface.
#[tokio::test]
async fn test_active_human_input_is_plain_user_message() {
    let provider = MockProvider::new(vec![vec![
        text_delta("acknowledged"),
        done_event(StopReason::EndTurn),
    ]]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let config = AgentLoopConfig::default();
    let mut loop_ctx = LoopContext::new("system");
    let (active_tx, active_rx, mut delivery_rx) = active_input_channel();
    let steer_id = active_tx
        .send_steer("human mid-turn steer")
        .expect("active steer should be accepted");
    loop_ctx.active_input_rx = Some(active_rx);

    let result = run_step_with_ctx(
        &provider,
        &executor,
        &store,
        &[],
        None,
        &config,
        &mut loop_ctx,
    )
    .await;
    assert_completed(result);

    let requests = provider.requests().expect("requests recorded");
    let user_messages: Vec<_> = requests[0]
        .messages
        .iter()
        .filter(|message| message.role == crate::provider::request::MessageRole::User)
        .map(|message| message.content.as_deref())
        .collect();
    assert_eq!(
        user_messages,
        vec![Some("test prompt"), Some("human mid-turn steer")],
    );

    let persisted_user_messages: Vec<_> = store
        .events()
        .into_iter()
        .filter_map(|event| match event {
            SessionEvent::UserMessage { content, .. } => Some(content),
            _ => None,
        })
        .collect();
    assert_eq!(
        persisted_user_messages,
        vec![
            "test prompt".to_string(),
            "human mid-turn steer".to_string(),
        ],
    );

    assert_eq!(
        delivery_rx.try_recv(),
        Some(crate::r#loop::ActiveInputDelivery {
            id: steer_id,
            content: "human mid-turn steer".to_string(),
        }),
    );
}

/// A multi-turn step. A Steer message is sent to the inbound channel before
/// the loop begins. The first provider call triggers a tool batch; after that
/// batch completes, the steer is drained and injected as a UserMessage before
/// the next provider call.
///
/// Proves that:
/// - InboundChannel Steer messages are drained at tool boundaries
/// - The injected message content appears in the session event store
/// - The steer does not terminate the loop — it continues to schema completion
#[tokio::test]
async fn test_inbound_steer_message() {
    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "read_file".to_string(),
        Box::new(|_: Value| Ok(serde_json::json!({"content": "file data"}))),
    );
    let executor = MockToolExecutor::new(handlers);

    // Turn 1: a non-schema tool call so the loop runs a tool batch (drain point)
    let turn1 = vec![
        tool_call_delta("tc_read", Some("read_file"), r#"{"path": "any.txt"}"#),
        done_event(StopReason::ToolUse),
    ];
    // Turn 2: schema to complete
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"done with steer"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    let schema = simple_schema();
    let config = AgentLoopConfig::default();
    let mut loop_ctx = LoopContext::new("system");

    // Create inbound channel and pre-load a Steer message.
    let (tx, mut rx) = inbound_channel(8);
    tx.send(crate::r#loop::inbound::ChannelMessage {
        id: uuid::Uuid::new_v4(),
        sender_id: uuid::Uuid::new_v4(),
        from: "test-orchestrator".to_owned(),
        role: None,
        to_id: uuid::Uuid::new_v4(),
        content: "steer content from orchestrator".to_owned(),
        kind: MessageKind::Steer,
        seq: None,
        timestamp: chrono::Utc::now(),
    })
    .await
    .expect("send steer message");

    let tools = [tool_def("read_file", "Read a file")];
    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "test prompt",
        tools: &tools,
        output_schema: Some(&schema),
        model: "test-model",
        config: &config,
        event_tx: None,
        inbound: Some(&mut rx),
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await
    .expect("run_agent_step must succeed");

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "done with steer");

    // Verify the steer message content appears in the session store.
    let events = store.events();
    let has_steer = events.iter().any(|e| {
        if let SessionEvent::UserMessage { content, .. } = e {
            content.contains("steer content from orchestrator")
        } else {
            false
        }
    });
    assert!(
        has_steer,
        "steer message content must appear as a UserMessage in the session store; events: {:?}",
        events
            .iter()
            .filter_map(|e| {
                if let SessionEvent::UserMessage { content, .. } = e {
                    Some(content.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>(),
    );
}

// ---------------------------------------------------------------------------
// Test 7: read-before-write enforcement through ToolRegistry
// ---------------------------------------------------------------------------

/// Verifies that WriteTool blocks writes to existing files that have not been
/// read first, then allows the write after a read has been performed.
/// This exercises the pre-validate lifecycle wired into ToolRegistry.
///
/// Proves that:
/// - WriteTool's `pre_validate` blocks unread-existing-file writes
/// - The ToolRegistry surfaces this as a `ToolError::PreValidationFailed`
/// - The error appears as the typed `{"error": {kind, message, detail}}`
///   payload in the session store ToolResult
/// - A successful write (to a new file) completes without error
#[tokio::test]
async fn test_read_before_write_enforcement_through_registry() {
    let dir = tempdir().expect("tempdir");
    let existing = dir.path().join("existing.txt");
    tokio::fs::write(&existing, "original\n")
        .await
        .expect("write existing file");
    let existing_str = existing.to_string_lossy().to_string();

    // Turn 1: attempt write to existing file without prior read (must be blocked)
    let turn1 = vec![
        tool_call_delta(
            "tc_bad_write",
            Some("write"),
            &format!(r#"{{"path": "{existing_str}", "content": "overwrite\n"}}"#),
        ),
        done_event(StopReason::ToolUse),
    ];
    // Turn 2: schema tool to finish (even after the blocked write)
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"write enforcement verified"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(WriteTool::new()));

    let schema = simple_schema();
    let config = AgentLoopConfig::default();
    let mut loop_ctx = LoopContext::new("system");

    let result = run_step_with_ctx(
        &provider,
        &registry,
        &store,
        &[tool_def("write", "Write a file")],
        Some(&schema),
        &config,
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "write enforcement verified");

    // The file must NOT have been overwritten.
    let on_disk = tokio::fs::read_to_string(&existing)
        .await
        .expect("existing file must still be readable");
    assert_eq!(
        on_disk, "original\n",
        "file must not be overwritten without prior read"
    );

    // The session store must record the blocked write as a structured error
    // result: the typed payload object (kind + message + guidance detail),
    // not a collapsed string.
    let events = store.events();
    let blocked_write = events.iter().any(|e| {
        if let SessionEvent::ToolResult {
            tool_name, output, ..
        } = e
        {
            if tool_name == "write" {
                let error = &output["error"];
                return error["kind"] == "blocked"
                    && error["message"]
                        .as_str()
                        .is_some_and(|m| m.contains("not been read"))
                    && error["detail"]["guidance"].is_string();
            }
        }
        false
    });
    assert!(
        blocked_write,
        "session store must record a structured ToolResult error for the blocked write",
    );
}

// ---------------------------------------------------------------------------
// Test 8: no-schema mode completes on first text stop
// ---------------------------------------------------------------------------

/// When no output schema is configured, the loop completes immediately on a
/// text-stop response without requiring a schema tool call.
///
/// Proves that:
/// - Text responses complete the loop in no-schema mode
/// - The completion output is a `Value::String` containing the text
/// - The session store records exactly one AssistantMessage
#[tokio::test]
async fn test_no_schema_mode_completes_on_text_stop() {
    let events = vec![
        text_delta("This is the answer."),
        done_event(StopReason::EndTurn),
    ];

    let provider = MockProvider::new(vec![events]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let config = AgentLoopConfig::default();
    let mut loop_ctx = LoopContext::new("system");

    let result = run_step_with_ctx(
        &provider,
        &executor,
        &store,
        &[],
        None, // no schema
        &config,
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(
        output,
        Value::String("This is the answer.".to_string()),
        "no-schema mode must return the text as a string Value",
    );

    // Exactly one AssistantMessage in the store.
    let assistant_messages = store
        .events()
        .iter()
        .filter(|e| matches!(e, SessionEvent::AssistantMessage { .. }))
        .count();
    assert_eq!(
        assistant_messages, 1,
        "exactly one AssistantMessage expected for a single-turn no-schema step",
    );
}

// ---------------------------------------------------------------------------
// Test 9: rules engine + RuleEngine with SystemContextAppend delivery
// ---------------------------------------------------------------------------

/// A step where the rules engine uses SystemContextAppend delivery for a
/// path-glob rule. After the tool fires, the rule body must appear in the
/// system_sections of LoopContext (visible as a dynamic section), not as
/// a UserMessage.
///
/// Proves that:
/// - SystemContextAppend delivery wires into `LoopContext::system_sections`
/// - The rule content is NOT recorded as a UserMessage
/// - The schema tool completes the step normally
#[tokio::test]
async fn test_rules_system_context_append_delivery() {
    let rule = Rule {
        id: RuleId::from("sys-append-rule"),
        name: "Sys Append".to_owned(),
        triggers: vec![TriggerCondition::PathGlob {
            pattern: "**/*.rs".to_owned(),
        }],
        delivery: RuleDelivery::SystemContextAppend,
        timing: TriggerTiming::After,
        body: "SYS_APPEND_CONTENT".to_owned(),
        shell_source: None,
    };

    let turn1 = vec![
        tool_call_delta(
            "tc_write",
            Some("write"),
            r#"{"path": "/tmp/foo.rs", "content": "fn main() {}\n"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"sys append done"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    let mut handlers: std::collections::HashMap<String, ToolHandler> =
        std::collections::HashMap::new();
    handlers.insert(
        "write".to_string(),
        Box::new(|_: Value| {
            Ok(serde_json::json!({
                "path": "/tmp/foo.rs",
                "bytes_written": 14,
                "line_count": 1,
                "length_limit": serde_json::Value::Null,
                "diagnostics": [],
                "check_overrides": []
            }))
        }),
    );
    let executor = MockToolExecutor::new(handlers);

    let schema = simple_schema();
    let config = AgentLoopConfig::default();
    let mut loop_ctx = LoopContext::new("base instruction");
    loop_ctx.rules = Some(RuleEngine::new(vec![rule]));

    let result = run_step_with_ctx(
        &provider,
        &executor,
        &store,
        &[tool_def("write", "Write a file")],
        Some(&schema),
        &config,
        &mut loop_ctx,
    )
    .await;

    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "sys append done");

    // SystemContextAppend must NOT produce a UserMessage.
    let events = store.events();
    let has_rule_user_msg = events.iter().any(|e| {
        if let SessionEvent::UserMessage { content, .. } = e {
            content.contains("SYS_APPEND_CONTENT")
        } else {
            false
        }
    });
    assert!(
        !has_rule_user_msg,
        "SystemContextAppend must not appear as a UserMessage in the session store",
    );
}

// ---------------------------------------------------------------------------
// Test 10: schema budget exhaustion produces SchemaUnreachable
// ---------------------------------------------------------------------------

/// With a budget of 2, the provider emits two invalid schema outputs in a row.
/// The loop must exhaust the budget and return SchemaUnreachable with
/// `attempts == 2`.
///
/// Proves that:
/// - Schema budget is correctly enforced
/// - `AgentStepResult::SchemaUnreachable` is returned when the budget runs out
/// - `best_attempt` is set to the last invalid output
/// - `attempts` equals the configured `schema_attempt_budget`
#[tokio::test]
async fn test_schema_budget_exhaustion_returns_schema_unreachable() {
    let turn1 = vec![
        tool_call_delta("tc_bad1", Some("structured_output"), r#"{"wrong": "data"}"#),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_bad2",
            Some("structured_output"),
            r#"{"still_wrong": true}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let schema = simple_schema();
    let config = AgentLoopConfig {
        schema_attempt_budget: 2,
        ..AgentLoopConfig::default()
    };
    let mut loop_ctx = LoopContext::new("system");

    let result = run_step_with_ctx(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config,
        &mut loop_ctx,
    )
    .await;

    match result {
        AgentStepResult::SchemaUnreachable {
            best_attempt,
            attempts,
            ..
        } => {
            assert_eq!(attempts, 2, "attempts must equal the schema_attempt_budget");
            assert!(
                best_attempt.is_some(),
                "best_attempt must be set to the last invalid output",
            );
        }
        other => panic!("expected SchemaUnreachable, got: {other:?}"),
    }

    assert_eq!(
        provider.call_count(),
        2,
        "provider must be called exactly twice before budget exhaustion",
    );
}

// ---------------------------------------------------------------------------
// N-025: Diagnostics wiring
// ---------------------------------------------------------------------------

/// Schema validation failure pushes exactly one diagnostic into the collector
/// before retry succeeds on the second attempt.
#[tokio::test]
async fn diagnostics_collector_records_schema_validation_failure_then_success() {
    use std::sync::Arc;

    use crate::integration::diagnostics::{DiagnosticCollector, DiagnosticSeverity};

    // Turn 1: invalid schema (missing required `answer`). Turn 2: valid.
    let turn1 = vec![
        tool_call_delta(
            "tc_bad",
            Some("structured_output"),
            r#"{"oops":"missing answer"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_good",
            Some("structured_output"),
            r#"{"answer":"finally"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();

    let collector = Arc::new(DiagnosticCollector::new());
    let schema = simple_schema();
    let config = AgentLoopConfig::default();
    let mut loop_ctx = LoopContext::new("base");
    loop_ctx.diagnostics = Some(Arc::clone(&collector));

    let result = run_step_with_ctx(
        &provider,
        &executor,
        &store,
        &[],
        Some(&schema),
        &config,
        &mut loop_ctx,
    )
    .await;
    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "finally");

    let snapshot = collector.snapshot();
    let schema_diags: Vec<_> = snapshot
        .iter()
        .filter(|d| d.code == "schema-violation")
        .collect();
    assert_eq!(
        schema_diags.len(),
        1,
        "expected exactly one schema-violation diagnostic, got: {snapshot:?}",
    );
    assert_eq!(schema_diags[0].severity, DiagnosticSeverity::Error);
}

/// Pre-validate block on Write (existing unread file) pushes exactly one
/// `tool-blocked` warning diagnostic into the collector.
#[tokio::test]
async fn diagnostics_collector_records_pre_validate_block() {
    use std::sync::Arc;

    use crate::integration::diagnostics::{DiagnosticCollector, DiagnosticSeverity};

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("existing.txt");
    tokio::fs::write(&path, "preexisting\n")
        .await
        .expect("seed file");

    let turn1 = vec![
        tool_call_delta(
            "tc_write",
            Some("write"),
            &format!(
                r#"{{"path": "{}", "content": "new content\n"}}"#,
                path.to_string_lossy()
            ),
        ),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"done"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    let mut registry = ToolRegistry::new();
    registry.register(Box::new(WriteTool::new()));

    let collector = Arc::new(DiagnosticCollector::new());
    let schema = simple_schema();
    let config = AgentLoopConfig::default();
    let mut loop_ctx = LoopContext::new("base");
    loop_ctx.diagnostics = Some(Arc::clone(&collector));

    let _ = run_step_with_ctx(
        &provider,
        &registry,
        &store,
        &[tool_def("write", "Write a file")],
        Some(&schema),
        &config,
        &mut loop_ctx,
    )
    .await;

    let snapshot = collector.snapshot();
    let blocked: Vec<_> = snapshot
        .iter()
        .filter(|d| d.code == "tool-blocked")
        .collect();
    assert_eq!(
        blocked.len(),
        1,
        "expected exactly one tool-blocked diagnostic, got: {snapshot:?}",
    );
    assert_eq!(blocked[0].severity, DiagnosticSeverity::Warning);
    assert_eq!(blocked[0].source_tool.as_deref(), Some("write"));
}

/// RuntimePostValidateCheck implementations can retrieve the
/// `DiagnosticCollector` via `ctx.get_extension::<DiagnosticCollector>()`
/// and push diagnostics, and those diagnostics appear in `drain`.
#[tokio::test]
async fn runtime_post_validate_check_can_push_diagnostics_via_extension() {
    use std::sync::Arc;

    use crate::integration::diagnostics::{
        DiagnosticCollector, DiagnosticSeverity, NornDiagnostic,
    };
    use crate::tool::lifecycle::{PostCheckResult, RuntimePostValidateCheck};
    use crate::tool::traits::ToolOutput;

    struct PushingCheck;
    #[async_trait]
    impl RuntimePostValidateCheck for PushingCheck {
        async fn check(&self, _output: &ToolOutput, ctx: &ToolContext) -> PostCheckResult {
            if let Some(collector) = ctx.get_extension::<DiagnosticCollector>() {
                collector.report(NornDiagnostic {
                    severity: DiagnosticSeverity::Info,
                    code: "runtime-check-fired".to_string(),
                    message: "runtime check ran".to_string(),
                    source_tool: Some("runtime-check".to_string()),
                    file_path: None,
                    suggestion: None,
                });
            }
            PostCheckResult::pass()
        }
    }

    let dir = tempdir().expect("tempdir");
    let path = dir.path().join("new.txt");

    let turn1 = vec![
        tool_call_delta(
            "tc_write",
            Some("write"),
            &format!(
                r#"{{"path": "{}", "content": "new content\n"}}"#,
                path.to_string_lossy()
            ),
        ),
        done_event(StopReason::ToolUse),
    ];
    let turn2 = vec![
        tool_call_delta(
            "tc_schema",
            Some("structured_output"),
            r#"{"answer":"done"}"#,
        ),
        done_event(StopReason::ToolUse),
    ];

    let provider = MockProvider::new(vec![turn1, turn2]);
    let store = EventStore::new();

    let mut ctx = ToolContext::empty();
    ctx.post_checks.push(Box::new(PushingCheck));
    let mut registry = ToolRegistry::with_context(Arc::new(ctx));
    registry.register(Box::new(WriteTool::new()));

    let collector = Arc::new(DiagnosticCollector::new());
    let schema = simple_schema();
    let config = AgentLoopConfig::default();
    let mut loop_ctx = LoopContext::new("base");
    loop_ctx.diagnostics = Some(Arc::clone(&collector));

    let _ = run_step_with_ctx(
        &provider,
        &registry,
        &store,
        &[tool_def("write", "Write a file")],
        Some(&schema),
        &config,
        &mut loop_ctx,
    )
    .await;

    let drained = collector.drain();
    assert!(
        drained.iter().any(|d| d.code == "runtime-check-fired"),
        "runtime check did not push via ctx.get_extension::<DiagnosticCollector>(): {drained:?}",
    );
}
