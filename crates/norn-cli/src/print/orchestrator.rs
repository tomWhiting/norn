//! End-to-end print-mode orchestrator (NC-003 R4 / R9).
//!
//! This is the first brief that actually calls into libnorn's
//! [`norn::agent_loop::runner::run_agent_step`]. It owns:
//!
//! 1. **Stdin handling** ([`compose_prompt`]): auto-detect piped stdin,
//!    read it in full, and prepend it to any positional `PROMPT` using
//!    the brief's `<stdin>` delimiters.
//! 2. **Output-schema parsing**: inline JSON when the value starts with
//!    `{`, otherwise a file path — both via the
//!    [`crate::event_schemas::parse_inline_or_file`] helper.
//! 3. **Provider construction**: dispatched through [`crate::provider::build_provider`].
//! 4. **Runtime wiring**: via [`crate::runtime::build_runtime`] which
//!    already wires token estimator, context edits, retry policy, and
//!    NC-003 R3 augmentations (diagnostics + iteration monitor).
//! 5. **Session persistence**: empty store on a fresh run, populated
//!    when `--resume` / `--fork` is supplied. Events are flushed to disk
//!    by the attached `JsonlSink` (write-through). The sink is
//!    index-registered: it accumulates the matching `index.jsonl` delta
//!    (event count, token totals, `updated_at`) per persisted event and
//!    flushes it at `EventStore::checkpoint` — which the orchestrator
//!    calls after the turn and after `/compact` — so the orchestrator
//!    never hand-reconciles the index.
//! 6. **Output dispatch**: text / json / stream-json (per
//!    [`crate::cli::OutputFormat`]).
//!
//! The result of every path is an [`crate::exit::ExitCode`] which the
//! binary converts into the OS process exit code.

use std::io::{IsTerminal, Read};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use norn::agent_loop::config::AgentStepResult;
use norn::error::{NornError, ProviderError};
use norn::provider::usage::Usage;
use norn::session::events::SessionEvent;
use norn::session::store::EventStore;
use serde_json::Value;
use tokio::sync::broadcast;

use super::output::{
    JsonEnvelope, UsageOut, drain_diagnostics, emit_stream_completed, extract_output_and_usage,
    render_json, render_text, result_label, spawn_stream_renderer,
};
use super::provider::build_provider;
use crate::cli::BuildError;
use crate::cli::ExitCode;
use crate::cli::{Cli, OutputFormat};
use crate::commands::slash::{
    DispatchOutcome, apply_clear_request, apply_compact_request, dispatch_input,
};
use crate::config::parse_inline_or_file;
use crate::runtime::build_runtime;
use crate::runtime::build_slash_state_with_schema;
use norn::tools::lsp::build_lsp_backend;

use crate::runtime::{
    RuntimeBundle, RuntimeInputs, apply_system_prompt, install_agent_tool_infra,
    register_standard_tools,
};
use crate::session::SessionPersistError;
use uuid::Uuid;

use super::session::open_session;

/// Buffer size for the streaming-event broadcast channel. Sized so a
/// brief burst of provider events does not push a slow consumer into
/// `Lagged`.
const BROADCAST_BUFFER_CAPACITY: usize = 256;

/// Entry point used by `main.rs::run_print`. Spins up a multi-threaded
/// tokio runtime and dispatches to [`run_async`].
///
/// # Errors
///
/// Returns the exit code in lieu of an error — see [`ExitCode`] for the
/// mapping.
#[must_use]
pub fn run(cli: &Cli) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("norn: failed to build tokio runtime: {err}");
            return ExitCode::AgentError;
        }
    };
    runtime.block_on(run_async(cli))
}

/// Async print-mode body. Public so integration tests can drive it from
/// inside an existing tokio runtime.
pub async fn run_async(cli: &Cli) -> ExitCode {
    match execute(cli).await {
        Ok(code) => code,
        Err(err) => report(&err),
    }
}

/// Errors that surface from the print orchestrator. Each variant maps
/// cleanly onto an [`ExitCode`] via [`PrintError::exit_code`].
#[derive(Debug, thiserror::Error)]
pub enum PrintError {
    /// Bad CLI argument — flag parsing or runtime assembly rejected the
    /// invocation (exit code 2).
    #[error("argument error: {0}")]
    Argument(String),
    /// Authentication failure (exit code 3).
    #[error("auth error: {0}")]
    Auth(String),
    /// Agent-runtime failure: provider call, tool error, schema budget
    /// exhausted, etc. (exit code 1).
    #[error("agent error: {0}")]
    Agent(String),
    /// Filesystem / I/O failure when reading stdin or writing output
    /// (exit code 1 — treated as an agent error per CO5).
    #[error("I/O error: {0}")]
    Io(String),
    /// Session persistence failed (exit code 1).
    #[error("session error: {0}")]
    Session(String),
}

impl PrintError {
    /// Terminal exit code per CO5.
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        match self {
            Self::Argument(_) => ExitCode::ArgumentError,
            Self::Auth(_) => ExitCode::AuthError,
            Self::Agent(_) | Self::Io(_) | Self::Session(_) => ExitCode::AgentError,
        }
    }
}

impl From<BuildError> for PrintError {
    fn from(err: BuildError) -> Self {
        match err {
            BuildError::Argument(msg) => Self::Argument(msg),
            BuildError::Auth(msg) => Self::Auth(msg),
        }
    }
}

impl From<std::io::Error> for PrintError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<SessionPersistError> for PrintError {
    fn from(err: SessionPersistError) -> Self {
        Self::Session(err.to_string())
    }
}

impl From<NornError> for PrintError {
    fn from(err: NornError) -> Self {
        if let NornError::Provider(ref provider_err) = err
            && matches!(provider_err, ProviderError::AuthenticationFailed { .. })
        {
            return Self::Auth(err.to_string());
        }
        Self::Agent(err.to_string())
    }
}

fn report(err: &PrintError) -> ExitCode {
    eprintln!("norn: {err}");
    err.exit_code()
}

/// Read stdin if it's piped, then dispatch to the orchestrator core.
async fn execute(cli: &Cli) -> Result<ExitCode, PrintError> {
    let stdin_content = read_stdin_if_piped()?;
    let positional = cli.prompt.join(" ");
    let effective_prompt = compose_prompt(stdin_content.as_deref(), &positional);

    let output_schema = parse_output_schema(cli.output_schema.as_deref())?;

    let mut inputs = RuntimeInputs::default();
    let lsp_backend = build_lsp_backend();
    register_standard_tools(&mut inputs.registry, Some(lsp_backend));
    let mut bundle = build_runtime(cli, inputs)?;
    apply_system_prompt(&mut bundle, norn::system_prompt::ExecutionMode::Headless);
    orchestrate(cli, bundle, effective_prompt, output_schema).await
}

/// Read stdin in full when it is not a TTY. Returns [`None`] when stdin
/// is a TTY (print mode invoked from a terminal with `-p`).
fn read_stdin_if_piped() -> Result<Option<String>, PrintError> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(None);
    }
    let mut buf = String::new();
    stdin.lock().read_to_string(&mut buf)?;
    Ok(Some(buf))
}

/// Build the effective prompt given an optional piped-stdin payload and
/// the positional `PROMPT` words joined into a single string.
///
/// Logic per NC-003 R4:
/// - `stdin = None`: return the positional prompt verbatim.
/// - `stdin = Some`, positional empty: use stdin verbatim.
/// - both present: wrap stdin in `<stdin>…</stdin>` and concatenate.
#[must_use]
pub fn compose_prompt(stdin: Option<&str>, positional: &str) -> String {
    match (stdin, positional.is_empty()) {
        (None, _) => positional.to_owned(),
        (Some(content), true) => content.to_owned(),
        (Some(content), false) => {
            format!("<stdin>\n{content}\n</stdin>\n\n{positional}")
        }
    }
}

/// Parse `-s` / `--output-schema` if provided. Failures are mapped to
/// [`PrintError::Argument`] so they surface as exit code 2.
fn parse_output_schema(raw: Option<&str>) -> Result<Option<Value>, PrintError> {
    let Some(value) = raw else { return Ok(None) };
    let parsed = parse_inline_or_file(value)?;
    Ok(Some(parsed))
}

async fn orchestrate(
    cli: &Cli,
    mut bundle: RuntimeBundle,
    prompt: String,
    output_schema: Option<Value>,
) -> Result<ExitCode, PrintError> {
    let session = open_session(cli, &bundle)?;
    crate::runtime::install_action_log(&bundle.registry, &session.store, &mut bundle.loop_context);
    let session_id = session.id().map(str::to_owned);
    bundle.agent_config.cache_key = session_id.clone();
    if let Some(env) = bundle.loop_context.environment.as_mut() {
        env.session_id.clone_from(&session_id);
    }
    if let Some(ref dir) = bundle.provider_overrides.debug_dump_dir {
        let file_name = session_id.as_deref().unwrap_or("unnamed");
        bundle.provider_overrides.debug_dump_file = Some(dir.join(format!("{file_name}.jsonl")));
    }
    let pre_event_count = session.store.len();

    // Build the slash-command surface and install the merged registry
    // on the loop context so profile-registered commands still fire
    // inside `run_agent_step`. The CLI builtins are intercepted by the
    // dispatcher above before reaching the loop, so their stderr side
    // effects never double-fire.
    let (slash_state, slash_registry) = build_slash_state_with_schema(
        cli,
        &bundle,
        Arc::clone(&session.store),
        session_id.clone(),
        output_schema,
    );
    bundle.loop_context.slash_commands = Some(slash_registry.clone());

    let outcome = match dispatch_input(&prompt, &slash_registry) {
        Ok(out) => out,
        Err(err) => return Err(PrintError::Agent(err.to_string())),
    };

    // Apply action flags raised by the closures. /compact performs a
    // real `ContextEdits::auto_compact_keeping_recent_turns` against the
    // live store; /clear replaces the in-memory store (the JSONL on
    // disk is unaffected); /exit short-circuits with success.
    if let Some(outcome) = apply_compact_request(&mut bundle, &session.store, &slash_state)? {
        outcome.log_to_stderr();
        // Flush the sink's pending index delta so the Compaction event is
        // reflected in index.jsonl even when this invocation returns
        // before the post-turn checkpoint (e.g. a bare `/compact` prompt).
        checkpoint_session(&session.store)?;
    }
    if apply_clear_request(&slash_state) {
        tracing::debug!("conversation cleared via /clear in print mode");
    }

    if slash_state.exit_requested.swap(false, Ordering::Relaxed) {
        return Ok(ExitCode::Success);
    }

    let format = cli.output_format.unwrap_or(OutputFormat::Text);

    let effective_prompt = match outcome {
        DispatchOutcome::HandledLocally => {
            write_handled_locally(cli, format, &bundle, session.id())?;
            return Ok(ExitCode::Success);
        }
        DispatchOutcome::PassToAgent(text) => text,
    };

    let active_schema = slash_state.output_schema_snapshot();
    let active_model = slash_state.model_snapshot();

    let built_provider = build_provider(cli.provider, &bundle.provider_overrides, &active_model)
        .await
        .map_err(|err| match err.exit_code() {
            ExitCode::AuthError => PrintError::Auth(err.to_string()),
            _ => PrintError::Agent(err.to_string()),
        })?;

    // The CLI's deliberate coordination envelope: child policy for every
    // spawn/fork plus the result-channel capacity, published alongside the
    // infra so spawn-time policy reads resolve (W3.2).
    let envelope = crate::runtime::cli_coordination_envelope();
    let (child_tx, child_rx) = tokio::sync::mpsc::channel::<
        norn::agent::result_channel::ChildAgentResult,
    >(envelope.child_result_capacity);
    let child_sender = norn::agent::result_channel::ChildResultSender(Arc::new(child_tx));
    bundle.loop_context.child_result_rx = Some(child_rx);

    // Register a depth-0 root agent so the agent registry has the same
    // shape headless as it does in the TUI. The single `root_id` is then
    // used for `AgentToolInfra.agent_id` and the root `AgentEventSender`
    // below, so spawned children record a real parent id rather than a
    // throwaway UUID or the nil UUID.
    let agent_registry = norn::agent::registry::AgentRegistry::shared();
    let root_id = register_root_agent(&agent_registry, &active_model)?;
    // W3.7 root inbound wiring: the returned receiver is the root's
    // inbound channel — its sender is registered in the MessageRouter
    // under `root_id`, so a child's `signal_agent(to: "parent")` lands
    // here. The orchestrator owns the receiver for the whole run and
    // threads it into the root step below; the route is process-lifetime
    // (never deregistered — the router lazily removes it on the first
    // delivery after this function returns and drops the receiver).
    let mut root_inbound = install_agent_tool_infra(
        &bundle.registry,
        built_provider.as_arc(),
        Arc::clone(&session.store),
        root_id,
        Arc::clone(&bundle.registry),
        agent_registry,
        envelope,
    );
    crate::runtime::install_child_result_sender(&bundle.registry, child_sender);
    // Headless reclamation: print mode has no agent status panel, so once
    // a child's result is delivered through the channel above, its
    // terminal registry entry and parent-held handle are reclaimed by the
    // launch wrapper. The TUI driver deliberately does NOT install this —
    // its status panel owns reclamation through the hold window.
    crate::runtime::install_headless_reclamation(&bundle.registry);

    // NH-006 R8 / C60: fire SessionLifecycleHook::on_session_start once
    // the runtime has been fully assembled and the HookRegistry is live
    // on bundle.loop_context.hooks. Programmatic trait-based lifecycle
    // hooks may be registered even when no shell hooks are configured.
    crate::runtime::wiring::run_session_start(
        bundle.loop_context.hooks.as_ref(),
        session_id.as_deref().unwrap_or(""),
    )
    .await;

    let tools = collect_tool_definitions(&bundle);

    let current_prompt = effective_prompt;
    let final_exit_code;

    {
        let (tx, _rx) = broadcast::channel::<norn::provider::AgentEvent>(BROADCAST_BUFFER_CAPACITY);
        let root_sender =
            norn::provider::AgentEventSender::new(tx.clone(), root_id, "root".to_string());
        crate::runtime::install_shared_agent_event_channel(&bundle.registry, tx.clone());
        let stream_renderer = if matches!(format, OutputFormat::StreamJson) {
            Some(spawn_stream_renderer(&tx, cli.partial))
        } else {
            None
        };

        let executor: &dyn norn::agent_loop::runner::ToolExecutor = &*bundle.registry;
        let result =
            norn::agent_loop::runner::run_agent_step(norn::agent_loop::runner::AgentStepRequest {
                provider: built_provider.as_dyn(),
                executor,
                store: &session.store,
                user_prompt: &current_prompt,
                tools: &tools,
                output_schema: active_schema.as_ref(),
                model: &active_model,
                config: &bundle.agent_config,
                event_tx: Some(&root_sender),
                // The root's inbound channel from install_agent_tool_infra:
                // child→root messages drain at this step's boundaries
                // through the framed <agent_message> injection path.
                inbound: root_inbound.as_mut(),
                loop_context: &mut bundle.loop_context,
                cancel: None,
            })
            .await;

        drop(root_sender);
        drop(tx);
        // REVIEW C1: the registry's shared ToolContext still holds the
        // SharedAgentEventChannel sender installed above (subagent event
        // forwarding), so the broadcast channel never closes here.
        // finish() signals the renderer explicitly; it drains the events
        // already buffered and exits instead of awaiting closure forever.
        // A JoinError (renderer panic or cancellation) means the streamed
        // output on stdout is incomplete or torn — that must not exit 0
        // with a clean `completed` envelope, so it surfaces on stderr via
        // the PrintError path and degrades the exit code.
        if let Some(handle) = stream_renderer
            && let Err(err) = handle.finish().await
        {
            return Err(renderer_failure(&err));
        }

        let result = match result {
            Ok(value) => value,
            Err(err) => {
                return Err(err.into());
            }
        };

        let diagnostics = drain_diagnostics(&bundle.diagnostics);
        // The attached `JsonlSink` already wrote every event of this turn
        // through to disk (write-through) and — being index-registered —
        // accumulated the matching index delta (event count, token
        // totals). Appending or hand-reconciling here would double-write
        // events (breaking `SessionManager::resume` on the duplicate-ID
        // guard) or
        // double-count the index; the orchestrator only checkpoints the
        // store so the sink flushes its own pending delta now rather
        // than at drop. The slice is collected only for the output
        // envelope.
        checkpoint_session(&session.store)?;
        let new_events = collect_new_events(&session.store, pre_event_count);

        let (output, usage) = extract_output_and_usage(&result);
        let label = result_label(&result);
        slash_state.add_usage(usage.clone());

        final_exit_code = match &result {
            AgentStepResult::Completed { .. } => ExitCode::Success,
            // Cancelled rides with the other non-completion outcomes for
            // CLI exit-code purposes — the shell sees a non-zero exit.
            // Structured workflow callers (Rhai) read the AgentStepResult
            // value directly and distinguish Cancelled from the others
            // there (S2).
            AgentStepResult::SchemaUnreachable { .. }
            | AgentStepResult::MaxIterationsReached { .. }
            | AgentStepResult::TimedOut { .. }
            | AgentStepResult::Cancelled { .. }
            | AgentStepResult::Truncated { .. } => ExitCode::AgentError,
        };

        let step = StepOutput {
            output: output.as_ref(),
            usage: &usage,
            model: &active_model,
            session_id: session.id(),
            events: &new_events,
            result: label,
            diagnostics: &diagnostics,
        };
        write_output(cli, format, &step)?;
    }

    // NH-006 R8 / C61: SessionLifecycleHook::on_session_end fires on the
    // single normal-exit path. Errors return early above and skip this
    // hook by design — the brief's acceptance does not require firing
    // on panic, and explicit cleanup is preferred over a drop guard.
    crate::runtime::wiring::run_session_end(
        bundle.loop_context.hooks.as_ref(),
        session_id.as_deref().unwrap_or(""),
    )
    .await;

    drop(built_provider);
    Ok(final_exit_code)
}

/// Render the "no agent call" output for a dispatch that was handled
/// locally. For `text` mode this is a no-op (the closure already wrote
/// to stderr); for `json` it produces a minimal envelope with no model
/// output; for `stream-json` it emits a single `completed` event.
fn write_handled_locally(
    cli: &Cli,
    format: OutputFormat,
    bundle: &RuntimeBundle,
    session_id: Option<&str>,
) -> Result<(), PrintError> {
    let usage = Usage::default();
    let diagnostics: Vec<norn::integration::NornDiagnostic> = Vec::new();
    match format {
        OutputFormat::Text => Ok(()),
        OutputFormat::Json => {
            let envelope = JsonEnvelope {
                output: None,
                usage: UsageOut::from(&usage),
                model: &bundle.model,
                session_id,
                events: &[],
                result: "completed",
                diagnostics: &diagnostics,
            };
            if let Some(path) = cli.output.as_ref() {
                let mut file = std::fs::File::create(path)?;
                render_json(&mut file, &envelope)?;
            } else {
                let mut stdout = std::io::stdout().lock();
                render_json(&mut stdout, &envelope)?;
            }
            Ok(())
        }
        OutputFormat::StreamJson => {
            if let Some(path) = cli.output.as_ref() {
                let mut file = std::fs::File::create(path)?;
                emit_stream_completed(&mut file, None, &usage, "completed", &diagnostics)?;
            } else {
                let mut stdout = std::io::stdout().lock();
                emit_stream_completed(&mut stdout, None, &usage, "completed", &diagnostics)?;
            }
            Ok(())
        }
    }
}

/// Register a depth-0 root agent in the shared [`AgentRegistry`], mirroring
/// the TUI driver, so headless runs expose the same agent hierarchy (a
/// known root for spawned children to parent against). Returns the
/// registry-assigned id.
///
/// # Errors
///
/// Returns [`PrintError::Agent`] when the registry rejects the reservation
/// or confirmation (e.g. a duplicate root path within the process).
fn register_root_agent(
    registry: &Arc<parking_lot::RwLock<norn::agent::registry::AgentRegistry>>,
    model: &str,
) -> Result<Uuid, PrintError> {
    // The root entry carries the CLI envelope's child policy — the root's
    // own granted budget, the ground truth its spawn/fork reservations are
    // checked against (W3.4).
    let guard = norn::agent::registry::AgentRegistry::reserve(
        registry,
        "/root".to_string(),
        "lead".to_string(),
        model.to_string(),
        None,
        crate::runtime::cli_coordination_envelope().child_policy,
        None,
    )
    .map_err(|err| PrintError::Agent(err.to_string()))?;
    let id = guard.id();
    guard
        .confirm()
        .map_err(|err| PrintError::Agent(err.to_string()))?;
    Ok(id)
}

fn collect_tool_definitions(
    bundle: &RuntimeBundle,
) -> Vec<norn::provider::request::ToolDefinition> {
    norn::provider::collect_function_definitions(&bundle.registry, None)
}

/// Flush the store's persistence sink: pending durability work and the
/// sink's accumulated index delta land now instead of at drop. A no-op
/// for sink-less stores (`--no-session`).
fn checkpoint_session(store: &EventStore) -> Result<(), PrintError> {
    store
        .checkpoint()
        .map_err(|err| PrintError::Session(err.to_string()))
}

/// Map a stream-renderer [`tokio::task::JoinError`] (panic or
/// cancellation) onto the agent-error path: the NDJSON already written to
/// stdout is incomplete, so the run must surface the failure on stderr
/// and exit non-zero instead of emitting a clean `completed` envelope.
fn renderer_failure(err: &tokio::task::JoinError) -> PrintError {
    PrintError::Agent(format!(
        "stream renderer task failed ({kind}): {err}; streamed output on stdout is incomplete",
        kind = if err.is_panic() { "panic" } else { "cancelled" },
    ))
}

fn collect_new_events(store: &EventStore, since: usize) -> Vec<SessionEvent> {
    let all = store.events();
    if since >= all.len() {
        return Vec::new();
    }
    all[since..].to_vec()
}

/// Post-step output data bundled for the output writers. Eliminates
/// the `too_many_arguments` lint without sacrificing the named-field
/// clarity that the orchestrator needs.
struct StepOutput<'a> {
    output: Option<&'a Value>,
    usage: &'a norn::provider::usage::Usage,
    model: &'a str,
    session_id: Option<&'a str>,
    events: &'a [SessionEvent],
    result: &'static str,
    diagnostics: &'a [norn::integration::NornDiagnostic],
}

fn write_output(cli: &Cli, format: OutputFormat, step: &StepOutput<'_>) -> Result<(), PrintError> {
    match format {
        OutputFormat::Text => write_text(cli, step.output, step.diagnostics),
        OutputFormat::Json => write_json(cli, step),
        OutputFormat::StreamJson => {
            write_stream_completed(cli, step.output, step.usage, step.result, step.diagnostics)
        }
    }
}

fn write_text(
    cli: &Cli,
    output: Option<&Value>,
    diagnostics: &[norn::integration::NornDiagnostic],
) -> Result<(), PrintError> {
    if let Some(path) = cli.output.as_ref() {
        let mut file = std::fs::File::create(path)?;
        let mut stderr = std::io::stderr().lock();
        render_text(&mut file, &mut stderr, output, diagnostics, cli.quiet)?;
        return Ok(());
    }
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    render_text(&mut stdout, &mut stderr, output, diagnostics, cli.quiet)?;
    Ok(())
}

fn write_json(cli: &Cli, step: &StepOutput<'_>) -> Result<(), PrintError> {
    let envelope = JsonEnvelope {
        output: step.output,
        usage: UsageOut::from(step.usage),
        model: step.model,
        session_id: step.session_id,
        events: step.events,
        result: step.result,
        diagnostics: step.diagnostics,
    };
    if let Some(path) = cli.output.as_ref() {
        let mut file = std::fs::File::create(path)?;
        render_json(&mut file, &envelope)?;
        return Ok(());
    }
    let mut stdout = std::io::stdout().lock();
    render_json(&mut stdout, &envelope)?;
    Ok(())
}

fn write_stream_completed(
    cli: &Cli,
    output: Option<&Value>,
    usage: &norn::provider::usage::Usage,
    result: &'static str,
    diagnostics: &[norn::integration::NornDiagnostic],
) -> Result<(), PrintError> {
    if let Some(path) = cli.output.as_ref() {
        let mut file = std::fs::File::create(path)?;
        emit_stream_completed(&mut file, output, usage, result, diagnostics)?;
        return Ok(());
    }
    let mut stdout = std::io::stdout().lock();
    emit_stream_completed(&mut stdout, output, usage, result, diagnostics)?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn compose_prompt_no_stdin_returns_positional() {
        assert_eq!(compose_prompt(None, "hello"), "hello");
    }

    #[test]
    fn compose_prompt_stdin_only_returns_stdin_verbatim() {
        assert_eq!(compose_prompt(Some("data"), ""), "data");
    }

    #[test]
    fn compose_prompt_both_wraps_stdin_in_delimiters() {
        let prompt = compose_prompt(Some("data"), "Summarise");
        assert_eq!(prompt, "<stdin>\ndata\n</stdin>\n\nSummarise");
    }

    #[test]
    fn compose_prompt_handles_multiline_stdin() {
        let prompt = compose_prompt(Some("a\nb\nc"), "do it");
        assert!(prompt.starts_with("<stdin>\na\nb\nc\n</stdin>"));
        assert!(prompt.ends_with("do it"));
    }

    #[test]
    fn compose_prompt_no_stdin_no_positional_returns_empty() {
        assert_eq!(compose_prompt(None, ""), "");
    }

    #[test]
    fn parse_output_schema_returns_none_for_none_input() {
        let result = parse_output_schema(None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_output_schema_inline_json_parses() {
        let result = parse_output_schema(Some(r#"{"type":"object"}"#))
            .unwrap()
            .unwrap();
        assert_eq!(result, serde_json::json!({"type": "object"}));
    }

    #[test]
    fn parse_output_schema_invalid_inline_json_is_argument_error() {
        let err = parse_output_schema(Some("{invalid")).unwrap_err();
        match err {
            PrintError::Argument(_) => {}
            other => panic!("expected Argument, got {other:?}"),
        }
        assert_eq!(err.exit_code(), ExitCode::ArgumentError);
    }

    #[test]
    fn parse_output_schema_file_path_reads_and_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schema.json");
        std::fs::write(&path, r#"{"type":"string"}"#).unwrap();
        let result = parse_output_schema(Some(path.to_str().unwrap()))
            .unwrap()
            .unwrap();
        assert_eq!(result, serde_json::json!({"type": "string"}));
    }

    #[test]
    fn parse_output_schema_missing_file_is_argument_error() {
        let err = parse_output_schema(Some("/no/such/file.json")).unwrap_err();
        assert!(matches!(err, PrintError::Argument(_)));
    }

    #[test]
    fn print_error_exit_codes() {
        assert_eq!(
            PrintError::Argument("x".to_owned()).exit_code(),
            ExitCode::ArgumentError
        );
        assert_eq!(
            PrintError::Auth("x".to_owned()).exit_code(),
            ExitCode::AuthError
        );
        assert_eq!(
            PrintError::Agent("x".to_owned()).exit_code(),
            ExitCode::AgentError
        );
        assert_eq!(
            PrintError::Io("x".to_owned()).exit_code(),
            ExitCode::AgentError
        );
        assert_eq!(
            PrintError::Session("x".to_owned()).exit_code(),
            ExitCode::AgentError
        );
    }

    #[test]
    fn agent_step_result_exit_code_mapping() {
        use norn::provider::usage::Usage;

        let completed = AgentStepResult::Completed {
            output: serde_json::json!("done"),
            usage: Usage::default(),
        };
        assert_eq!(
            match &completed {
                AgentStepResult::Completed { .. } => ExitCode::Success,
                _ => ExitCode::AgentError,
            },
            ExitCode::Success
        );

        let schema = AgentStepResult::SchemaUnreachable {
            best_attempt: None,
            usage: Usage::default(),
            attempts: 0,
            validation_errors: Vec::new(),
        };
        assert_eq!(
            match &schema {
                AgentStepResult::Completed { .. } => ExitCode::Success,
                _ => ExitCode::AgentError,
            },
            ExitCode::AgentError
        );

        let max_iter = AgentStepResult::MaxIterationsReached {
            usage: Usage::default(),
        };
        assert_eq!(
            match &max_iter {
                AgentStepResult::Completed { .. } => ExitCode::Success,
                _ => ExitCode::AgentError,
            },
            ExitCode::AgentError
        );

        let timed_out = AgentStepResult::TimedOut {
            partial_output: None,
            elapsed: std::time::Duration::from_mins(1),
            iterations: 5,
            usage: Usage::default(),
        };
        assert_eq!(
            match &timed_out {
                AgentStepResult::Completed { .. } => ExitCode::Success,
                _ => ExitCode::AgentError,
            },
            ExitCode::AgentError
        );

        let cancelled = AgentStepResult::Cancelled {
            usage: Usage::default(),
        };
        assert_eq!(
            match &cancelled {
                AgentStepResult::Completed { .. } => ExitCode::Success,
                _ => ExitCode::AgentError,
            },
            ExitCode::AgentError
        );

        let truncated = AgentStepResult::Truncated {
            kind: norn::agent_loop::config::TruncationKind::MaxTokens,
            partial_text: Some("partial".to_string()),
            iterations: 1,
            usage: Usage::default(),
        };
        assert_eq!(
            match &truncated {
                AgentStepResult::Completed { .. } => ExitCode::Success,
                _ => ExitCode::AgentError,
            },
            ExitCode::AgentError
        );
    }

    /// A renderer `JoinError` (panic) must degrade to the agent-error exit
    /// path with a stderr-visible message — never a clean exit 0.
    #[tokio::test]
    async fn renderer_panic_maps_to_agent_error_exit() {
        let task = tokio::spawn(async {
            panic!("renderer blew up");
        });
        let join_err = task.await.expect_err("task must panic");
        let err = renderer_failure(&join_err);
        match &err {
            PrintError::Agent(message) => {
                assert!(message.contains("panic"), "message: {message}");
                assert!(message.contains("incomplete"), "message: {message}");
            }
            other => panic!("expected Agent, got {other:?}"),
        }
        assert_eq!(err.exit_code(), ExitCode::AgentError);
    }

    /// A cancelled renderer task is also a failure (output torn), mapped
    /// to the same degraded exit path with the cancellation named.
    #[tokio::test]
    async fn renderer_cancellation_maps_to_agent_error_exit() {
        let task = tokio::spawn(async {
            std::future::pending::<()>().await;
        });
        task.abort();
        let join_err = task.await.expect_err("task must be cancelled");
        let err = renderer_failure(&join_err);
        match &err {
            PrintError::Agent(message) => {
                assert!(message.contains("cancelled"), "message: {message}");
            }
            other => panic!("expected Agent, got {other:?}"),
        }
        assert_eq!(err.exit_code(), ExitCode::AgentError);
    }

    #[test]
    fn norn_error_authentication_failed_maps_to_auth() {
        let err: PrintError = NornError::Provider(ProviderError::AuthenticationFailed {
            reason: "expired".to_owned(),
        })
        .into();
        assert!(matches!(err, PrintError::Auth(_)));
    }

    #[test]
    fn norn_error_connection_failed_maps_to_agent() {
        let err: PrintError = NornError::Provider(ProviderError::ConnectionFailed {
            reason: "refused".to_owned(),
        })
        .into();
        assert!(matches!(err, PrintError::Agent(_)));
    }
}
