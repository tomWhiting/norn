//! Config-to-trait bridge for shell-command hooks.
//!
//! [`ShellCommandHook`] is the single struct that lifts a config entry into a
//! registrable trait implementation. One instance is constructed per hook
//! command entry in the merged [`crate::config::types::HookSettings`] and
//! implements all eleven hook traits — the trait the registry actually
//! dispatches is selected by the [`HookEventType`] stored on the instance.
//!
//! Dispatch flow for each trait method (with one fire-and-forget exception
//! for [`super::traits::SessionEventHook`]):
//!
//! 1. Gate on the matcher. Events whose
//!    [`HookEventType::supports_matcher`] is `false` always fire; otherwise
//!    the per-event matcher input (tool name, model name, session event
//!    variant name) is tested against the compiled [`HookMatcher`].
//! 2. Build the per-event [`HookInput`] from the dispatch arguments and the
//!    captured [`HookContext`].
//! 3. Spawn `sh -c <command>` via [`tokio::process::Command`], pipe the
//!    serialised [`HookInput`] to stdin, set the five `NORN_*` environment
//!    variables, and wait with [`tokio::time::timeout`].
//! 4. Interpret the exit code per `DESIGN.md` D11:
//!    - exit 0 + JSON stdout → [`HookOutput::to_hook_outcome`].
//!    - exit 0 + empty/invalid stdout → [`HookOutcome::Proceed`].
//!    - exit 2 → [`HookOutcome::Block`] with stderr as the reason.
//!    - any other exit code (or signal) → warn + [`HookOutcome::Proceed`].
//!    - spawn / I/O / timeout failures → warn + [`HookOutcome::Proceed`].
//!
//! [`super::traits::SessionEventHook`] is the only fire-and-forget impl (D15). It clones
//! the hook's state into a [`tokio::spawn`] task so a slow logger does not
//! bottleneck `append_and_notify` on the agent loop. All other impls await
//! the spawn inline.

use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::config::HookEventType;
use super::input::{
    HookInput, NORN_AGENT_ID, NORN_HOOK_EVENT, NORN_PROFILE, NORN_PROJECT_DIR, NORN_SESSION_ID,
};
use super::matchers::HookMatcher;
use super::output::HookOutput;
use super::traits::HookOutcome;
#[cfg(test)]
use super::traits::{PreToolHook, SessionEventHook};
use crate::session::events::SessionEvent;
#[cfg(test)]
use crate::tool::context::ToolContext;
use crate::tool::envelope::ToolEnvelope;
use crate::tool::traits::ToolOutput;

mod implementations;

/// Common context fields captured at hook construction time.
///
/// `profile_name` is a plain [`String`] with the empty-string sentinel for
/// "no profile" (matches [`NORN_PROFILE`] and [`HookInput::profile_name`]).
/// All four fields are owned so the hook can be cloned into a
/// fire-and-forget [`tokio::spawn`] task without borrowing from caller
/// state.
#[derive(Clone, Debug)]
pub struct HookContext {
    /// Current session identifier (set on stdin and as `NORN_SESSION_ID`).
    pub session_id: String,
    /// Working directory the child process inherits (`NORN_PROJECT_DIR`).
    pub cwd: String,
    /// Current agent identifier (`NORN_AGENT_ID`).
    pub agent_id: String,
    /// Active profile name. Empty string means "no profile active".
    pub profile_name: String,
}

/// Config-driven hook bridge that implements every hook trait via shell.
///
/// One instance per parsed [`crate::config::types::HookEntry`]. The
/// stored [`HookEventType`] selects which trait method is the active one —
/// the other ten implementations are effectively no-ops because the hook is
/// only registered under the variant that matches `event_type`.
#[derive(Clone, Debug)]
pub struct ShellCommandHook {
    command: String,
    matcher: HookMatcher,
    timeout: Duration,
    event_type: HookEventType,
    context: HookContext,
}

impl ShellCommandHook {
    /// Construct a shell command hook bound to a single
    /// [`HookEventType`].
    ///
    /// The five arguments are all required. There is no [`Default`] —
    /// every field has operational meaning and a hardcoded default value
    /// would violate CO6 (timeout) and the design's no-assumed-defaults
    /// stance.
    #[must_use]
    pub const fn new(
        command: String,
        matcher: HookMatcher,
        timeout: Duration,
        event_type: HookEventType,
        context: HookContext,
    ) -> Self {
        Self {
            command,
            matcher,
            timeout,
            event_type,
            context,
        }
    }

    /// Gate check used before spawning the child process. Events whose
    /// `event_type` does not support a matcher always fire. Otherwise the
    /// matcher input — supplied per-event-type by the trait impl — must
    /// match the compiled [`HookMatcher`].
    fn should_fire(&self, matcher_input: Option<&str>) -> bool {
        if !self.event_type.supports_matcher() {
            return true;
        }
        match matcher_input {
            Some(input) => self.matcher.matches(input),
            // event_type supports a matcher but the caller did not supply
            // one — treat as no-match to fail safe.
            None => false,
        }
    }

    /// Render a [`HookInput`] populated with the four common fields and the
    /// `snake_case` event name for this hook's event type. Per-event extras
    /// are filled in by the caller before sending.
    fn base_input(&self) -> HookInput {
        HookInput {
            session_id: self.context.session_id.clone(),
            cwd: self.context.cwd.clone(),
            hook_event_name: event_type_name(self.event_type).to_owned(),
            agent_id: self.context.agent_id.clone(),
            profile_name: self.context.profile_name.clone(),
            tool_name: None,
            tool_input: None,
            tool_call_id: None,
            tool_output: None,
            tool_duration_ms: None,
            tool_is_error: None,
            model: None,
            message_count: None,
            final_text: None,
            subagent_id: None,
            subagent_type: None,
        }
    }

    fn tool_result_input(&self, envelope: &ToolEnvelope, output: &ToolOutput) -> HookInput {
        let mut input = self.base_input();
        input.tool_name = Some(envelope.tool_name.clone());
        input.tool_input = Some(envelope.model_args.clone());
        input.tool_call_id = Some(envelope.tool_call_id.clone());
        input.tool_output = Some(output.content.clone());
        input.tool_duration_ms =
            Some(u64::try_from(output.duration.as_millis()).unwrap_or(u64::MAX));
        input
    }

    /// Core spawn pipeline shared by every trait impl except the fire-and-
    /// forget [`super::traits::SessionEventHook`] entry point.
    ///
    /// Returns [`HookOutcome::Proceed`] on every non-fatal failure path
    /// (spawn error, I/O error, timeout, malformed JSON) per `DESIGN.md`
    /// D11 — the agent loop never wedges because a hook script misbehaved.
    async fn execute(&self, input: HookInput) -> HookOutcome {
        let payload = match serde_json::to_string(&input) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(
                    command = %self.command,
                    error = %err,
                    "failed to serialise hook input; treating as proceed",
                );
                return HookOutcome::Proceed;
            }
        };

        let governor = match crate::resource::DescriptorGovernor::global() {
            Ok(governor) => governor,
            Err(error) => {
                tracing::warn!(
                    command = %self.command,
                    %error,
                    "shell hook descriptor admission unavailable; treating as proceed",
                );
                return HookOutcome::Proceed;
            }
        };
        let _permit = match governor.try_acquire(crate::resource::THREE_PIPE_SPAWN_PEAK) {
            Ok(permit) => permit,
            Err(error) => {
                tracing::warn!(
                    command = %self.command,
                    %error,
                    "shell hook descriptor admission failed; treating as proceed",
                );
                return HookOutcome::Proceed;
            }
        };

        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(&self.command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&self.context.cwd)
            .env(NORN_PROJECT_DIR, &self.context.cwd)
            .env(NORN_SESSION_ID, &self.context.session_id)
            .env(NORN_AGENT_ID, &self.context.agent_id)
            .env(NORN_PROFILE, &self.context.profile_name)
            .env(NORN_HOOK_EVENT, event_type_name(self.event_type))
            .kill_on_drop(true);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(err) => {
                tracing::warn!(
                    command = %self.command,
                    error = %err,
                    "failed to spawn shell hook; treating as proceed",
                );
                return HookOutcome::Proceed;
            }
        };

        let Some(mut stdin) = child.stdin.take() else {
            tracing::warn!(
                command = %self.command,
                "child stdin handle missing; treating as proceed",
            );
            let _ = child.start_kill();
            return HookOutcome::Proceed;
        };
        if let Err(err) = stdin.write_all(payload.as_bytes()).await {
            tracing::warn!(
                command = %self.command,
                error = %err,
                "failed to write hook input to child stdin; treating as proceed",
            );
            // Drop stdin so the child observes EOF, then best-effort kill so
            // we don't leak a half-fed process if it stalls on stdin.
            drop(stdin);
            let _ = child.start_kill();
            return HookOutcome::Proceed;
        }
        // Explicit drop signals EOF to the child reader.
        drop(stdin);

        let wait = child.wait_with_output();
        let output = match tokio::time::timeout(self.timeout, wait).await {
            Ok(Ok(output)) => output,
            Ok(Err(err)) => {
                tracing::warn!(
                    command = %self.command,
                    error = %err,
                    "failed to wait on hook child; treating as proceed",
                );
                return HookOutcome::Proceed;
            }
            Err(_) => {
                tracing::warn!(
                    command = %self.command,
                    timeout_secs = self.timeout.as_secs_f64(),
                    "hook command timed out; killed child, treating as proceed",
                );
                return HookOutcome::Proceed;
            }
        };

        interpret_exit(
            &self.command,
            self.event_type,
            output.status.code(),
            &output.stdout,
            &output.stderr,
        )
    }
}

/// `Snake_case` wire form of a [`HookEventType`] — matches the serde rendering
/// in [`super::config`] and the `NORN_HOOK_EVENT` env-var value spec from
/// `DESIGN.md` D14.
const fn event_type_name(event_type: HookEventType) -> &'static str {
    match event_type {
        HookEventType::PreTool => "pre_tool",
        HookEventType::PostTool => "post_tool",
        HookEventType::PostToolFailure => "post_tool_failure",
        HookEventType::PreLlm => "pre_llm",
        HookEventType::PostLlm => "post_llm",
        HookEventType::SessionEvent => "session_event",
        HookEventType::UserPrompt => "user_prompt",
        HookEventType::Stop => "stop",
        HookEventType::SubagentStart => "subagent_start",
        HookEventType::SubagentStop => "subagent_stop",
        HookEventType::SessionStart => "session_start",
        HookEventType::SessionEnd => "session_end",
        HookEventType::PreCompaction => "pre_compaction",
    }
}

/// Discriminator-name of a [`SessionEvent`] variant used as the matcher
/// input for `session_event` hooks (D17 / C51). Returned as a `&'static str`
/// to avoid an allocation in the hot path.
const fn session_event_variant_name(event: &SessionEvent) -> &'static str {
    match event {
        SessionEvent::UserMessage { .. } => "UserMessage",
        SessionEvent::AssistantMessage { .. } => "AssistantMessage",
        SessionEvent::SpokenResponse { .. } => "SpokenResponse",
        SessionEvent::ToolResult { .. } => "ToolResult",
        SessionEvent::ModelChange { .. } => "ModelChange",
        SessionEvent::ProviderEpochBoundary { .. } => "ProviderEpochBoundary",
        SessionEvent::Compaction { .. } => "Compaction",
        SessionEvent::ChildBranch { .. } => "ChildBranch",
        SessionEvent::ForkComplete { .. } => "ForkComplete",
        SessionEvent::Label { .. } => "Label",
        SessionEvent::Custom { .. } => "Custom",
        SessionEvent::ContextMark { .. } => "ContextMark",
        SessionEvent::RuleInjection { .. } => "RuleInjection",
    }
}

/// Apply the `DESIGN.md` D11 exit-code protocol to a finished child's
/// output. Pulled into a free function so unit tests can exercise it
/// without spawning a real subprocess.
fn interpret_exit(
    command: &str,
    event_type: HookEventType,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> HookOutcome {
    match code {
        Some(0) => {
            if stdout.iter().all(u8::is_ascii_whitespace) {
                return HookOutcome::Proceed;
            }
            match serde_json::from_slice::<HookOutput>(stdout) {
                Ok(parsed) => parsed.to_hook_outcome(event_type),
                Err(err) => {
                    tracing::warn!(
                        command = %command,
                        error = %err,
                        "hook stdout was not valid JSON; treating as proceed",
                    );
                    HookOutcome::Proceed
                }
            }
        }
        Some(2) => {
            let reason = String::from_utf8_lossy(stderr).into_owned();
            HookOutcome::Block { reason }
        }
        Some(other) => {
            let stderr_text = String::from_utf8_lossy(stderr);
            tracing::warn!(
                command = %command,
                exit_code = other,
                stderr = %stderr_text,
                "hook exited non-zero (non-block); treating as proceed",
            );
            HookOutcome::Proceed
        }
        None => {
            // Killed by signal — treat as a transient script failure.
            let stderr_text = String::from_utf8_lossy(stderr);
            tracing::warn!(
                command = %command,
                stderr = %stderr_text,
                "hook terminated by signal; treating as proceed",
            );
            HookOutcome::Proceed
        }
    }
}

#[cfg(test)]
#[allow(
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
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn ctx() -> HookContext {
        HookContext {
            session_id: "sess-1".to_owned(),
            cwd: "/tmp".to_owned(),
            agent_id: "agent-1".to_owned(),
            profile_name: String::new(),
        }
    }

    fn envelope(name: &str) -> ToolEnvelope {
        ToolEnvelope {
            tool_call_id: "tc_1".to_owned(),
            tool_name: name.to_owned(),
            model_args: serde_json::json!({"k": "v"}),
            metadata: serde_json::Value::Null,
        }
    }

    // R2 acceptance: shell hook receives JSON on stdin. Drive the spawn by
    // writing stdin to a temp file and verify the file contents are the
    // serialised HookInput.
    #[tokio::test]
    async fn pre_tool_hook_writes_json_to_stdin() {
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        let path = tmp.path().to_path_buf();
        let cmd = format!("cat > {}", path.display());
        let hook = ShellCommandHook::new(
            cmd,
            HookMatcher::new(None).unwrap(),
            Duration::from_secs(5),
            HookEventType::PreTool,
            ctx(),
        );

        let outcome = hook
            .before_tool(&envelope("Write"), &ToolContext::empty())
            .await;
        assert!(matches!(outcome, HookOutcome::Proceed));

        let body = std::fs::read_to_string(&path).expect("read temp");
        let parsed: serde_json::Value = serde_json::from_str(body.trim()).expect("json");
        assert_eq!(parsed["session_id"], "sess-1");
        assert_eq!(parsed["hook_event_name"], "pre_tool");
        assert_eq!(parsed["tool_name"], "Write");
        assert_eq!(parsed["tool_call_id"], "tc_1");
        assert_eq!(parsed["tool_input"], serde_json::json!({"k": "v"}));
    }

    // R3 acceptance: command = 'sleep 60' with 1s timeout returns Proceed
    // within ~1s.
    #[tokio::test]
    async fn timeout_kills_long_running_hook_and_returns_proceed() {
        let hook = ShellCommandHook::new(
            "sleep 60".to_owned(),
            HookMatcher::new(None).unwrap(),
            Duration::from_secs(1),
            HookEventType::PreTool,
            ctx(),
        );
        let start = Instant::now();
        let outcome = hook
            .before_tool(&envelope("Write"), &ToolContext::empty())
            .await;
        let elapsed = start.elapsed();
        assert!(matches!(outcome, HookOutcome::Proceed));
        assert!(
            elapsed < Duration::from_secs(5),
            "expected timeout near 1s, elapsed = {elapsed:?}",
        );
    }

    // R4 acceptance: exit 0 with block JSON → Block { reason }.
    #[tokio::test]
    async fn exit_zero_with_block_json_returns_block() {
        let hook = ShellCommandHook::new(
            r#"cat >/dev/null; printf '{"decision":"block","reason":"test"}'"#.to_owned(),
            HookMatcher::new(None).unwrap(),
            Duration::from_secs(5),
            HookEventType::PreTool,
            ctx(),
        );
        let outcome = hook
            .before_tool(&envelope("Write"), &ToolContext::empty())
            .await;
        match outcome {
            HookOutcome::Block { reason } => assert_eq!(reason, "test"),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    // R4 acceptance: exit 2 with stderr → Block { reason: stderr }.
    #[tokio::test]
    async fn exit_two_returns_block_with_stderr_reason() {
        let hook = ShellCommandHook::new(
            "cat >/dev/null; printf denied 1>&2; exit 2".to_owned(),
            HookMatcher::new(None).unwrap(),
            Duration::from_secs(5),
            HookEventType::PreTool,
            ctx(),
        );
        let outcome = hook
            .before_tool(&envelope("Write"), &ToolContext::empty())
            .await;
        match outcome {
            HookOutcome::Block { reason } => assert_eq!(reason, "denied"),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    // R4 acceptance: exit 1 → Proceed (warning logged).
    #[tokio::test]
    async fn exit_one_returns_proceed() {
        let hook = ShellCommandHook::new(
            "cat >/dev/null; exit 1".to_owned(),
            HookMatcher::new(None).unwrap(),
            Duration::from_secs(5),
            HookEventType::PreTool,
            ctx(),
        );
        let outcome = hook
            .before_tool(&envelope("Write"), &ToolContext::empty())
            .await;
        assert!(matches!(outcome, HookOutcome::Proceed));
    }

    // R4 acceptance: exit 0 with empty stdout → Proceed.
    #[tokio::test]
    async fn exit_zero_with_empty_stdout_returns_proceed() {
        let hook = ShellCommandHook::new(
            "cat >/dev/null; exit 0".to_owned(),
            HookMatcher::new(None).unwrap(),
            Duration::from_secs(5),
            HookEventType::PreTool,
            ctx(),
        );
        let outcome = hook
            .before_tool(&envelope("Write"), &ToolContext::empty())
            .await;
        assert!(matches!(outcome, HookOutcome::Proceed));
    }

    // R4 + interpret_exit unit coverage: invalid JSON on exit 0 → Proceed.
    #[test]
    fn interpret_exit_invalid_json_returns_proceed() {
        let outcome = interpret_exit("noop", HookEventType::PreTool, Some(0), b"not json", b"");
        assert!(matches!(outcome, HookOutcome::Proceed));
    }

    // R4: exit 2 with empty stderr still returns Block with empty reason.
    #[test]
    fn interpret_exit_two_with_empty_stderr_blocks_with_empty_reason() {
        let outcome = interpret_exit("noop", HookEventType::PreTool, Some(2), b"", b"");
        match outcome {
            HookOutcome::Block { reason } => assert!(reason.is_empty()),
            other => panic!("expected Block, got {other:?}"),
        }
    }

    // R4: signal-killed child (code == None) → Proceed.
    #[test]
    fn interpret_exit_signal_returns_proceed() {
        let outcome = interpret_exit("noop", HookEventType::PreTool, None, b"", b"sigterm");
        assert!(matches!(outcome, HookOutcome::Proceed));
    }

    // R5 acceptance: ShellCommandHook with matcher 'Write' does NOT fire
    // for tool name 'Read'. The command would block; instead it must short-
    // circuit to Proceed without ever spawning.
    #[tokio::test]
    async fn matcher_miss_returns_proceed_without_spawning() {
        // A command that would unambiguously Block if it ran.
        let cmd = "cat >/dev/null; printf denied 1>&2; exit 2".to_owned();
        let hook = ShellCommandHook::new(
            cmd,
            HookMatcher::new(Some("Write")).unwrap(),
            Duration::from_secs(5),
            HookEventType::PreTool,
            ctx(),
        );
        let outcome = hook
            .before_tool(&envelope("Read"), &ToolContext::empty())
            .await;
        // If the spawn had happened, exit 2 would surface as Block; the
        // matcher must short-circuit first.
        assert!(matches!(outcome, HookOutcome::Proceed));
    }

    // R7 acceptance: slow shell session_event hook does not block the
    // calling async context. The on_event call must return immediately
    // even though the spawned task sleeps for 1s with kill_on_drop.
    #[tokio::test]
    async fn session_event_fire_and_forget_returns_quickly() {
        use crate::session::events::EventBase;

        let hook = ShellCommandHook::new(
            "cat >/dev/null; sleep 1".to_owned(),
            HookMatcher::new(None).unwrap(),
            Duration::from_secs(5),
            HookEventType::SessionEvent,
            ctx(),
        );
        let event = SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "hi".to_owned(),
        };
        let start = Instant::now();
        hook.on_event(&event).await;
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "session_event hook must not block; elapsed = {elapsed:?}",
        );
    }
}
