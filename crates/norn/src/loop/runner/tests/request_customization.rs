use super::*;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

// -- N-020 R4: reasoning_effort threads through to ProviderRequest --

/// Capture the most recent provider request, exposing its
/// `reasoning_effort` field for assertion.
struct CaptureReasoning {
    observed: std::sync::Arc<parking_lot::Mutex<Option<crate::provider::request::ReasoningEffort>>>,
}

#[async_trait::async_trait]
impl crate::integration::hooks::PreLlmHook for CaptureReasoning {
    async fn before_llm(
        &self,
        request: &crate::provider::request::ProviderRequest,
    ) -> crate::integration::hooks::HookOutcome {
        *self.observed.lock() = request.reasoning_effort;
        crate::integration::hooks::HookOutcome::Proceed
    }
}

/// N-020 R4: When `loop_context.reasoning_effort` is set, the
/// `ProviderRequest` constructed by the loop must carry that value.
#[tokio::test]
async fn reasoning_effort_threads_to_provider_request() {
    use crate::integration::hooks::{Hook, HookRegistry};
    use crate::provider::request::ReasoningEffort;

    let turn = vec![
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"answer":"ok"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let observed: std::sync::Arc<parking_lot::Mutex<Option<ReasoningEffort>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(None));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreLlm(Box::new(CaptureReasoning {
        observed: std::sync::Arc::clone(&observed),
    })));

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.reasoning_effort = Some(ReasoningEffort::Low);
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
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
    let captured = *observed.lock();
    assert_eq!(
        captured,
        Some(ReasoningEffort::Low),
        "ProviderRequest must carry the LoopContext's reasoning_effort",
    );
}

struct CaptureServiceTier {
    observed: std::sync::Arc<parking_lot::Mutex<Option<crate::provider::request::ServiceTier>>>,
}

#[async_trait::async_trait]
impl crate::integration::hooks::PreLlmHook for CaptureServiceTier {
    async fn before_llm(
        &self,
        request: &crate::provider::request::ProviderRequest,
    ) -> crate::integration::hooks::HookOutcome {
        *self.observed.lock() = request.service_tier;
        crate::integration::hooks::HookOutcome::Proceed
    }
}

#[tokio::test]
async fn service_tier_threads_to_provider_request() {
    use crate::integration::hooks::{Hook, HookRegistry};
    use crate::provider::request::ServiceTier;

    let turn = vec![
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"answer":"ok"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let observed: std::sync::Arc<parking_lot::Mutex<Option<ServiceTier>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(None));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreLlm(Box::new(CaptureServiceTier {
        observed: std::sync::Arc::clone(&observed),
    })));

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.service_tier = Some(ServiceTier::Fast);
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
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
    assert_eq!(*observed.lock(), Some(ServiceTier::Fast));
}

// -- N-020 R5: slash command expansion lands in provider messages --

/// Capture the messages on the most recent provider request so we can
/// assert the slash expansion replaced the literal `/command …` text.
struct CaptureMessages {
    observed: std::sync::Arc<parking_lot::Mutex<Vec<Message>>>,
}

#[async_trait::async_trait]
impl crate::integration::hooks::PreLlmHook for CaptureMessages {
    async fn before_llm(
        &self,
        request: &crate::provider::request::ProviderRequest,
    ) -> crate::integration::hooks::HookOutcome {
        *self.observed.lock() = request.messages.clone();
        crate::integration::hooks::HookOutcome::Proceed
    }
}

/// N-020 R5: A registered `/review foo.rs` slash command must expand
/// the literal user input into the handler's messages BEFORE the
/// provider call. The literal `/review foo.rs` text must not appear as
/// a `UserMessage` in the provider request.
#[tokio::test]
async fn slash_command_expands_before_provider_call() -> TestResult {
    use crate::integration::hooks::{Hook, HookRegistry};
    use crate::r#loop::commands::{SlashCommand, SlashCommandHandler, SlashCommandRegistry};

    let turn = vec![
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"answer":"ok"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let observed: std::sync::Arc<parking_lot::Mutex<Vec<Message>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreLlm(Box::new(CaptureMessages {
        observed: std::sync::Arc::clone(&observed),
    })));

    let mut slash = SlashCommandRegistry::new();
    slash.register(SlashCommand {
        name: "review".to_owned(),
        handler: SlashCommandHandler::Skill {
            skill_name: "review".to_owned(),
        },
    });

    let mut loop_ctx = LoopContext::new("system");
    loop_ctx.slash_commands = Some(slash);
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));

    let result = run_agent_step(AgentStepRequest {
        provider: &provider,
        executor: &executor,
        store: &store,
        user_prompt: "/review foo.rs",
        tools: &[],
        output_schema: Some(&schema),
        model: "test-model",
        config: &default_config(),
        event_tx: None,
        inbound: None,
        loop_context: &mut loop_ctx,
        cancel: None,
    })
    .await?;
    let (output, _) = assert_completed(result);
    assert_eq!(output["answer"], "ok");

    let messages = observed.lock().clone();
    // The literal `/review foo.rs` text must NOT appear in any user
    // message that hit the provider — the slash expansion must replace
    // it. The expansion contains both 'review' and 'foo.rs'.
    let user_bodies: Vec<String> = messages
        .iter()
        .filter(|m| m.role == MessageRole::User)
        .filter_map(|m| m.content.clone())
        .collect();
    assert!(
        !user_bodies.iter().any(|b| b == "/review foo.rs"),
        "literal /review must be replaced by expansion; got {user_bodies:?}",
    );
    assert!(
        user_bodies
            .iter()
            .any(|b| b.contains("review") && b.contains("foo.rs")),
        "expansion must reference both skill name and argument; got {user_bodies:?}",
    );
    Ok(())
}

// -- N-020 R6: prompt command stdout appears in system instruction --

/// Captured request snapshot: system message (messages[0]) content
/// and the managed dynamic-context Developer message (the tail message).
#[derive(Clone, Debug)]
struct CapturedTurn {
    system: String,
    dynamic: Option<String>,
}

/// Capture the System message and Developer message content on
/// each provider call.
struct CaptureSystemContent {
    captured: std::sync::Arc<parking_lot::Mutex<Vec<CapturedTurn>>>,
}

#[async_trait::async_trait]
impl crate::integration::hooks::PreLlmHook for CaptureSystemContent {
    async fn before_llm(
        &self,
        request: &crate::provider::request::ProviderRequest,
    ) -> crate::integration::hooks::HookOutcome {
        let system = request
            .messages
            .first()
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        // The managed dynamic-context Developer message is the LAST message
        // in the request (tail placement for prefix caching).
        let dynamic = request
            .messages
            .last()
            .filter(|m| matches!(m.role, MessageRole::Developer))
            .and_then(|m| m.content.clone());
        self.captured.lock().push(CapturedTurn { system, dynamic });
        crate::integration::hooks::HookOutcome::Proceed
    }
}

/// N-020 R6: a successful prompt command's stdout appears in the managed
/// dynamic-context Developer message (the tail message), not in the System
/// message (which stays stable for prefix caching).
#[tokio::test]
async fn prompt_command_appears_in_dynamic_context() -> TestResult {
    use crate::integration::hooks::{Hook, HookRegistry};
    use crate::profile::PromptCommand;

    let turn = vec![
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"answer":"ok"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let captured: std::sync::Arc<parking_lot::Mutex<Vec<CapturedTurn>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreLlm(Box::new(CaptureSystemContent {
        captured: std::sync::Arc::clone(&captured),
    })));

    let mut loop_ctx = LoopContext::new("base-system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));
    loop_ctx.prompt_commands.push(PromptCommand {
        name: "cwd".to_owned(),
        command: "echo Current dir: token-found".to_owned(),
        cache_ttl: None,
    });

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
            schema: Some(&schema),
            config: &default_config(),
            event_tx: None,
            inbound: None,
        },
        &mut loop_ctx,
    )
    .await;

    let _ = assert_completed(result);
    let snapshots = captured.lock().clone();
    assert!(!snapshots.is_empty(), "expected at least one provider call");
    assert_eq!(
        snapshots[0].system, "base-system",
        "system message must stay stable; got: {}",
        snapshots[0].system,
    );
    let dynamic = snapshots[0]
        .dynamic
        .as_ref()
        .ok_or_else(|| std::io::Error::other("managed Developer tail message was not present"))?;
    assert!(
        dynamic.contains("token-found"),
        "prompt command stdout must appear in dynamic context; got: {dynamic}",
    );
    assert!(
        dynamic.contains("cwd"),
        "prompt command name should appear as a section heading; got: {dynamic}",
    );
    Ok(())
}

/// N-020 R6: a failing prompt command (non-zero exit) is logged and
/// skipped — it must NOT abort the loop and must NOT add a section.
#[tokio::test]
async fn prompt_command_failure_skips_section_without_abort() {
    use crate::integration::hooks::{Hook, HookRegistry};
    use crate::profile::PromptCommand;

    let turn = vec![
        tool_call_delta("tc_schema", Some("structured_output"), r#"{"answer":"ok"}"#),
        done_event(StopReason::ToolUse),
    ];
    let provider = MockProvider::new(vec![turn]);
    let store = EventStore::new();
    let executor = MockToolExecutor::empty();
    let schema = simple_schema();

    let captured: std::sync::Arc<parking_lot::Mutex<Vec<CapturedTurn>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register(Hook::PreLlm(Box::new(CaptureSystemContent {
        captured: std::sync::Arc::clone(&captured),
    })));

    let mut loop_ctx = LoopContext::new("base-system");
    loop_ctx.hooks = Some(std::sync::Arc::new(hooks));
    loop_ctx.prompt_commands.push(PromptCommand {
        name: "bad".to_owned(),
        command: "exit 7".to_owned(),
        cache_ttl: None,
    });

    let result = run_step_with(
        StepArgs {
            provider: &provider,
            executor: &executor,
            store: &store,
            tools: &[],
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
    assert!(
        !snapshots.is_empty(),
        "loop must complete despite prompt-command failure",
    );
    assert_eq!(
        snapshots[0].system, "base-system",
        "failed prompt command must not append a section",
    );
    let dyn_content = snapshots[0].dynamic.as_deref().unwrap_or("");
    assert!(
        !dyn_content.contains("bad"),
        "failed prompt command must not add its section to the developer message; got: {dyn_content}",
    );
}
