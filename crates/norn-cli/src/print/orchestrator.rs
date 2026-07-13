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
//! 4. **Runtime wiring**: via [`builder_from_cli`](crate::runtime::builder_from_cli)
//!    → `AgentBuilder::build` → [`AgentParts`](norn::agent::AgentParts), which
//!    wires token estimator, context edits, retry policy, diagnostics, and
//!    the iteration monitor.
//! 5. **Session persistence**: empty store on a fresh run, populated
//!    when `--resume` / `--fork` is supplied. Events are flushed to disk
//!    by the attached `JsonlSink` (write-through). The sink is
//!    index-registered: it accumulates the matching `index.jsonl` delta
//!    (event count, token totals, `updated_at`) per persisted event and
//!    flushes it at `EventStore::checkpoint_off_executor` — which the
//!    orchestrator awaits after the turn and after `/compact` — so the
//!    orchestrator never hand-reconciles the index and never blocks an
//!    executor thread on the index lock.
//! 6. **Output dispatch**: text / json / stream-json (per
//!    [`crate::cli::OutputFormat`]), via [`super::step_output`].
//!
//! The driven duplex path (`--protocol jsonrpc`) is owned by
//! [`super::driven`] and specified in
//! `docs/design/norn-cli/DRIVEN-PROTOCOL.md`.
//!
//! The result of every path is an [`crate::exit::ExitCode`] which the
//! binary converts into the OS process exit code.

use std::io::{IsTerminal, Read};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use norn::agent::AgentParts;
use norn::agent::registry::AgentRegistry;
use norn::agent_loop::config::AgentStepResult;
use norn::agent_loop::runner::{AgentStepRequest, driver_executor, run_agent_step};
use norn::error::{NornError, ProviderError};
use norn::session::events::SessionEvent;
use norn::session::store::EventStore;
use norn::system_prompt::ExecutionMode;
use serde_json::Value;

use super::driven::{execute_driven, finish_intervene_loop, spawn_intervene_loop};
use super::jsonrpc::{DrivenRun, SharedRunDriver};
use super::output::{StopInfo, drain_diagnostics, extract_output_and_usage};
use super::provider::build_provider;
use super::step_output::{
    StepOutput, driven_result_value, emit_error_envelope, write_handled_locally, write_output,
};
use super::stream_renderer::spawn_stream_renderer;
use crate::cli::BuildError;
use crate::cli::ExitCode;
use crate::cli::{Cli, OutputFormat, Protocol};
use crate::commands::slash::{
    DispatchOutcome, apply_clear_request, apply_compact_request, dispatch_input,
};
use crate::config::parse_inline_or_file;
use crate::runtime::{
    SlashStateInputs, build_slash_state_with_schema, builder_from_cli, cli_coordination_envelope,
    resolve_invocation, warn_unmatched_tool_flag_names,
};
use crate::session::SessionPersistError;
use norn::tools::lsp::build_lsp_backend;

/// Buffer size for the streaming-event broadcast channel the builder
/// creates via `.event_channel_capacity`. Sized so a brief burst of
/// provider events does not push a slow consumer into `Lagged`.
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
            // R2: even this pre-runtime failure is post-argument-parsing,
            // so plain machine formats still get the typed error stop. A
            // jsonrpc peer expects JSON-RPC frames only, never a print
            // envelope, so driven mode is excluded. Known divergence: the
            // stderr line above is byte-frozen from before the envelope
            // existed and has no class prefix, while the envelope message
            // is the `PrintError` Display (`agent error: …`) like every
            // other emit site — see the `StopInfo::Error::message` doc.
            if cli.protocol != Some(Protocol::Jsonrpc) {
                emit_error_envelope(
                    cli,
                    &PrintError::Agent(format!("failed to build tokio runtime: {err}")),
                    None,
                    None,
                );
            }
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
    /// The stream renderer tore stdout mid-run — panic or cancellation —
    /// so the NDJSON already written is incomplete (exit code 1). Never
    /// followed by an error envelope: appending a well-formed terminal
    /// event to a torn stream would make the output look more
    /// trustworthy than it is (owner ruling R4, 2026-07-06). The Display
    /// prefix deliberately matches [`PrintError::Agent`] so the stderr
    /// line is unchanged from when this failure rode the `Agent` variant.
    #[error("agent error: {0}")]
    StreamTorn(String),
}

impl PrintError {
    /// Terminal exit code per CO5.
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        match self {
            Self::Argument(_) => ExitCode::ArgumentError,
            Self::Auth(_) => ExitCode::AuthError,
            Self::Agent(_) | Self::Io(_) | Self::Session(_) | Self::StreamTorn(_) => {
                ExitCode::AgentError
            }
        }
    }

    /// The machine-stable `stop.class` this failure carries on the typed
    /// error envelope, or `None` when the failure must stay stderr-only:
    /// argument errors keep clap parity (exit 2 — owner ruling R2) and a
    /// torn stream gets no envelope at all (owner ruling R4).
    #[must_use]
    pub const fn envelope_class(&self) -> Option<&'static str> {
        match self {
            Self::Argument(_) | Self::StreamTorn(_) => None,
            Self::Auth(_) => Some("auth"),
            Self::Agent(_) => Some("agent"),
            Self::Io(_) => Some("io"),
            Self::Session(_) => Some("session"),
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
///
/// When `--protocol jsonrpc` is set the driven duplex path is taken instead
/// (stdin is the JSON-RPC channel, never the prompt); every other format
/// and the TUI are unreached in that case.
async fn execute(cli: &Cli) -> Result<ExitCode, PrintError> {
    if cli.protocol == Some(Protocol::Jsonrpc) {
        return execute_driven(cli).await;
    }

    // Every failure from here on is post-argument-parsing: the machine
    // formats get the typed error envelope IN ADDITION to the stderr line
    // and the unchanged exit code (owner ruling R2, 2026-07-06). The
    // model and session id are not resolved yet at these sites, so the
    // envelope's nullable fields stay null.
    let stdin_content = read_stdin_if_piped().map_err(|err| fail_before_assembly(cli, err))?;
    let positional = cli.prompt.join(" ");
    let effective_prompt = compose_prompt(stdin_content.as_deref(), &positional);

    let output_schema = parse_output_schema(cli.output_schema.as_deref())
        .map_err(|err| fail_before_assembly(cli, err))?;

    let assembly = assemble_print_agent(cli)
        .await
        .map_err(|err| fail_before_assembly(cli, err))?;
    orchestrate(cli, assembly, effective_prompt, output_schema, None).await
}

/// The assembled print agent plus the driver-resolved configuration the
/// orchestrator still needs after assembly (values that live on
/// [`ResolvedInvocation`](crate::runtime::ResolvedInvocation) but have no
/// home on the library's `AgentParts`).
pub(super) struct PrintAssembly {
    /// The decomposed agent the step loop drives.
    pub parts: AgentParts,
    /// The resolved session index-lock deadline, applied by the slash
    /// surface to every lock-taking `SessionManager` it constructs
    /// (`/name`'s index rename).
    pub index_lock_deadline: std::time::Duration,
}

/// Route a plain-mode pre-assembly failure through the error-envelope
/// emitter and hand the error back unchanged for the stderr line + exit
/// code. Argument errors emit nothing (clap parity — R2); the emitter
/// filters by class. Driven mode never reaches this path — [`execute`]
/// branches into [`execute_driven`] first, and its post-acceptance
/// failures are answered as id-matched JSON-RPC error responses.
fn fail_before_assembly(cli: &Cli, err: PrintError) -> PrintError {
    emit_error_envelope(cli, &err, None, None);
    err
}

/// Assemble the headless print agent through the single library-owned
/// assembler: resolve the CLI invocation, build the provider up front from
/// the resolved model (the model still travels per-request through
/// `run_agent_step`, so `/model` keeps working), map the resolved state
/// onto the [`AgentBuilder`](norn::agent::AgentBuilder) via
/// [`builder_from_cli`], chain the CLI's headless coordination envelope,
/// build, and decompose into [`AgentParts`] the step-loop drives.
///
/// Terminal reclamation is `true` here: print mode has no agent status
/// panel, so a finished child's terminal registry entry and parent-held
/// handle are reclaimed once its result is delivered. (The TUI passes
/// `false` — its status panel owns reclamation.)
///
/// # Errors
///
/// [`PrintError::Argument`] / [`PrintError::Auth`] when resolution,
/// provider construction, or `build()` reject the invocation.
pub(super) async fn assemble_print_agent(cli: &Cli) -> Result<PrintAssembly, PrintError> {
    let resolved = resolve_invocation(cli)?;
    let index_lock_deadline = resolved.index_lock_deadline;

    // Debug-dump file naming (D4): the provider is built before the
    // session id is minted, so the dump file is named from the only
    // pre-resolvable identifier — the explicit `--session-id`, else the
    // `--session-name`, else `unnamed`. Debug-only; never load-bearing.
    let mut provider_overrides = resolved.provider_overrides;
    if let Some(dir) = provider_overrides.debug_dump_dir.clone() {
        let hint = cli
            .session_id
            .as_deref()
            .or(cli.session_name.as_deref())
            .unwrap_or("unnamed");
        norn::util::validate_private_component(hint, "debug dump session name")
            .map_err(|error| PrintError::Argument(error.to_string()))?;
        provider_overrides.debug_dump_file = Some(dir.join(format!("{hint}.jsonl")));
    }

    let built_provider =
        build_provider(resolved.provider_kind, &provider_overrides, &resolved.model)
            .await
            .map_err(|err| match err.exit_code() {
                ExitCode::AuthError => PrintError::Auth(err.to_string()),
                _ => PrintError::Agent(err.to_string()),
            })?;

    let envelope = cli_coordination_envelope(resolved.delegation_depth);
    let agent = builder_from_cli(
        cli,
        built_provider.as_arc(),
        resolved.profile,
        &resolved.settings,
        &resolved.applied,
    )?
    .execution_mode(ExecutionMode::Headless)
    .lsp_backend(build_lsp_backend().map_err(|error| PrintError::Agent(error.to_string()))?)
    .agent_registry(AgentRegistry::shared())
    .child_policy(envelope.child_policy.clone())
    .child_result_capacity(envelope.child_result_capacity)
    .event_channel_capacity(BROADCAST_BUFFER_CAPACITY)
    .inbound_capacity(envelope.child_policy.inbound_capacity)
    .register_root("/root".to_string(), "lead".to_string())
    .terminal_reclamation(true)
    .build()?;
    let parts = agent.into_parts();
    // Deferred until here (not inside `builder_from_cli`) because gating
    // happens during `build()`: the assembled registry is the authoritative
    // reference for which flag-named tools exist.
    warn_unmatched_tool_flag_names(&parts.registry, &resolved.applied);
    Ok(PrintAssembly {
        parts,
        index_lock_deadline,
    })
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
pub(super) fn parse_output_schema(raw: Option<&str>) -> Result<Option<Value>, PrintError> {
    let Some(value) = raw else { return Ok(None) };
    let parsed = parse_inline_or_file(value)?;
    Ok(Some(parsed))
}

/// Run the assembled agent and dispatch its output, wrapping the run so
/// that EVERY plain-mode failure — dispatch, compaction, checkpoint, the
/// step itself, output writing — additionally emits the typed error
/// envelope (owner ruling R2). The envelope is filtered by class inside
/// [`emit_error_envelope`]: argument errors and a torn stream stay
/// stderr-only (R2 / R4). Driven mode is excluded here and answers its
/// failures as the id-matched JSON-RPC error response in
/// [`execute_driven`] — emitting an envelope too would double-report on a
/// frame-only stdout.
pub(super) async fn orchestrate(
    cli: &Cli,
    assembly: PrintAssembly,
    prompt: String,
    output_schema: Option<Value>,
    driven_run: Option<DrivenRun>,
) -> Result<ExitCode, PrintError> {
    // Captured before the run consumes the assembly: an error envelope
    // names the model and session the failed run was assembled with (R3
    // keeps the payload minimal; the nullable fields stay null only when
    // genuinely unknown, i.e. pre-assembly).
    let model = assembly.parts.model.clone();
    let session_id: Option<String> = assembly
        .parts
        .session_entry
        .as_ref()
        .map(|entry| entry.id.clone());
    let is_driven = driven_run.is_some();

    let result = orchestrate_run(cli, assembly, prompt, output_schema, driven_run).await;

    if !is_driven && let Err(err) = result.as_ref() {
        emit_error_envelope(cli, err, Some(&model), session_id.as_deref());
    }
    result
}

/// The fallible body of [`orchestrate`]: stdin/slash dispatch, the agent
/// step, session checkpointing, and output dispatch. Kept separate so the
/// wrapper above can observe ANY failure exactly once.
async fn orchestrate_run(
    cli: &Cli,
    assembly: PrintAssembly,
    prompt: String,
    output_schema: Option<Value>,
    driven_run: Option<DrivenRun>,
) -> Result<ExitCode, PrintError> {
    let PrintAssembly {
        mut parts,
        index_lock_deadline,
    } = assembly;
    // Split the driven-mode context into the shared driver (result + events,
    // consulted throughout) and the stdin reader (consumed once, by the
    // mid-run intervene loop at step time). Keeping them apart lets the many
    // existing `driven.as_ref()` sites stay a cheap Option<&SharedRunDriver>.
    let (driven, driven_reader): (Option<SharedRunDriver>, Option<_>) = match driven_run {
        Some(DrivenRun { driver, reader }) => (Some(driver), Some(reader)),
        None => (None, None),
    };
    // The builder opened the session (`.open_session`), installed the
    // action log, and stamped the cache key, environment session id, and
    // debug-dump naming during `build()`; `AgentParts` hands back the same
    // store the loop persists into. A managed session carries a
    // `session_entry`; `--no-session` has none, so the output envelope's
    // session id is `None` there exactly as before.
    let store = Arc::clone(&parts.event_store);
    let output_session_id: Option<String> =
        parts.session_entry.as_ref().map(|entry| entry.id.clone());
    let pre_event_count = store.len();

    // Build the slash-command surface and install the merged registry
    // on the loop context so profile-registered commands still fire
    // inside `run_agent_step`. The CLI builtins are intercepted by the
    // dispatcher above before reaching the loop, so their stderr side
    // effects never double-fire.
    let (slash_state, slash_registry) = build_slash_state_with_schema(
        cli,
        SlashStateInputs {
            registry: &parts.registry,
            model: &parts.model,
            service_tier: parts.loop_context.service_tier,
            reasoning_effort: parts.loop_context.reasoning_effort,
        },
        Arc::clone(&store),
        output_session_id.clone(),
        index_lock_deadline,
        output_schema,
    )
    .map_err(|error| PrintError::Argument(error.to_string()))?;
    parts.loop_context.slash_commands = Some(slash_registry.clone());

    let outcome = match dispatch_input(&prompt, &slash_registry) {
        Ok(out) => out,
        Err(err) => return Err(PrintError::Agent(err.to_string())),
    };

    // Apply action flags raised by the closures. /compact performs a
    // real `ContextEdits::auto_compact_keeping_recent_turns` against the
    // live store; /clear rotates into a fresh sink-registered session
    // (the retired JSONL on disk is untouched); /exit short-circuits
    // with success.
    if let Some(outcome) = apply_compact_request(
        parts.config.auto_compact_keep_recent_turns,
        &mut parts.loop_context,
        &store,
        &slash_state,
    )? {
        outcome.log_to_stderr();
        // Flush the sink's pending index delta so the Compaction event is
        // reflected in index.jsonl even when this invocation returns
        // before the post-turn checkpoint (e.g. a bare `/compact` prompt).
        checkpoint_session(&store).await?;
    }
    // `/clear` carries the same sink discipline as startup (session-
    // fidelity Gap 12): on a persisted invocation the slash state
    // rotates into a fresh sink-registered session, so anything a
    // driver appends through `SlashState::current_store()` afterwards
    // is exactly as durable as pre-clear appends. The durability policy
    // is the CLI's explicit `Flush` choice — the same one the session
    // front door passes in `from_cli` (D4). From here on the reported
    // session id is the LIVE slash-state cell: after a rotation the
    // output envelope and the driven response must name the session the
    // events actually land in — a driver resuming by the retired id
    // would replay the full pre-clear history it asked to leave behind.
    let clear_report = apply_clear_and_report(&slash_state)?;
    let output_session_id = clear_report.envelope_session_id;
    if let Some(line) = clear_report.operator_line {
        eprintln!("{line}");
    }

    let format = cli.output_format.unwrap_or(OutputFormat::Text);

    // A locally-handled prompt is ALWAYS answered before any exit request
    // is honoured. The `/exit` and `/quit` CLI builtins resolve to
    // `HandledLocally` AND set `exit_requested` during dispatch; emitting
    // the local response first guarantees a driven-mode peer receives the
    // id-matched `run/execute` response (a null result) instead of EOF,
    // and plain `-f json`/`stream-json` still write the local envelope.
    // (`DRIVEN-PROTOCOL.md`: "Every accepted run/execute is answered ... a
    // prompt resolving entirely to a local slash command is answered with a
    // success Response whose result is null.") The old ordering checked
    // `exit_requested` first and short-circuited both, violating the
    // protocol for `/exit`/`/quit` prompts.
    let effective_prompt = match outcome {
        DispatchOutcome::HandledLocally => {
            // A run/execute whose prompt resolved to a local slash command
            // never runs the agent. In driven mode the run response is
            // still required (a null result), so the peer's request is not
            // left unanswered; otherwise render the local envelope.
            if let Some(driver) = driven.as_ref() {
                driver
                    .finish_with_result(Value::Null)
                    .map_err(|err| PrintError::Io(err.to_string()))?;
            } else {
                write_handled_locally(cli, format, &parts.model, output_session_id.as_deref())?;
            }
            return Ok(ExitCode::Success);
        }
        DispatchOutcome::PassToAgent(text) => text,
    };

    // Honour `/exit` / `/quit` for any command that set the flag but did
    // NOT resolve to a local dispatch. The CLI builtins are `HandledLocally`
    // and are answered above; this is the defensive terminal path for a
    // future exit-setting command whose text is passed to the agent.
    if slash_state.exit_requested.swap(false, Ordering::Relaxed) {
        return Ok(ExitCode::Success);
    }

    let active_schema = slash_state.output_schema_snapshot();
    let active_model = slash_state.model_snapshot();
    parts.loop_context.service_tier = slash_state.service_tier_snapshot();
    parts.loop_context.reasoning_effort = slash_state.reasoning_effort_snapshot();

    // Session-lifecycle hooks (D1 / R1.7): the `into_parts` step-loop
    // driver fires them explicitly around the run with the resolved
    // `info.session_id` — never the empty string the pre-migration path
    // passed on `--no-session`. `Agent::run` fires these itself; a custom
    // driver like this one uses the `AgentParts` helpers.
    parts.fire_session_start().await;

    let current_prompt = effective_prompt;
    let final_exit_code;

    {
        // The builder created the event broadcast channel and the root
        // sender (`.event_channel_capacity`) and published the shared
        // channel extension so fork/spawn children stream through it. A
        // missing channel is an assembly invariant violation, surfaced
        // rather than silently dropping every streamed event.
        let Some(tx) = parts.events_tx.clone() else {
            return Err(PrintError::Agent(
                "event broadcast channel missing after assembly (event_channel_capacity \
                 was not wired)"
                    .to_string(),
            ));
        };
        // Driven mode replaces the stream renderer with the live `event/*`
        // notification emitter subscribed off the SAME broadcast channel;
        // otherwise the stream renderer runs exactly as before. The two are
        // mutually exclusive — driven mode never enters the render path.
        let (stream_renderer, event_emitter) = if let Some(driver) = driven.as_ref() {
            (None, Some(driver.attach_emitter(&tx)))
        } else if matches!(format, OutputFormat::StreamJson) {
            (Some(spawn_stream_renderer(&tx, cli.partial)), None)
        } else {
            (None, None)
        };

        // Driven-mode WRITE direction: while the run is in flight,
        // concurrently read in-band `intervene/*` requests off the same
        // stdin reader and map them onto Norn's control channel — inject via
        // the harness router to the root, cancel via the builder's root
        // cancellation token (the same token published as `AgentCancellation`
        // so a cancel cascades to every spawned descendant). The reader task
        // is spawned only in driven mode; a plain CLI run has no reader.
        let (intervene_task, intervene_stop) = spawn_intervene_loop(
            driven.as_ref(),
            driven_reader,
            &parts.registry,
            parts.id,
            &parts.cancel,
        );

        // `driver_executor` coerces the registry to `Arc<dyn ToolExecutor>`,
        // passed below as `&Arc<dyn ToolExecutor>` (not `&*registry`) so the
        // loop's concurrent batch steps get an owned handle
        // (`ToolExecutor::owned_handle` via the blanket `Arc<dyn
        // ToolExecutor>` impl) and can spawn each batch member on its own
        // task for true parallelism — mirroring `Agent::run`
        // (`agent/instance.rs`), so the library and CLI drivers share
        // identical concurrent-batch semantics after assembly unification.
        let executor = driver_executor(&parts.registry);
        let result = run_agent_step(AgentStepRequest {
            provider: parts.provider.as_ref(),
            executor: &executor,
            store: &store,
            user_prompt: &current_prompt,
            tools: &parts.tool_defs,
            output_schema: active_schema.as_ref(),
            model: &active_model,
            config: &parts.config,
            event_tx: parts.event_sender.as_ref(),
            // The root's inbound channel the builder wired: child→root
            // messages drain at this step's boundaries through the framed
            // <agent_message> injection path.
            inbound: parts.inbound.as_mut(),
            loop_context: &mut parts.loop_context,
            cancel: Some(parts.cancel.clone()),
        })
        .await;

        // The run has ended (completed, cancelled, or errored). Signal the
        // intervene reader to stop and join it, so no reader task outlives
        // the run and every ack it emitted is accounted for before the
        // terminal result is sent. A reader already stopped (EOF or a cancel
        // it applied) makes the stop-send a no-op; join still completes.
        finish_intervene_loop(intervene_task, intervene_stop).await;

        drop(tx);
        // REVIEW C1: the registry's shared ToolContext still holds the
        // SharedAgentEventChannel sender the builder installed (subagent
        // event forwarding), so the broadcast channel never closes here.
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
        // Drain and stop the driven-mode event emitter before the run
        // response is sent, so every `event/*` notification is on the wire
        // ahead of the terminal result. A panic/cancellation means the live
        // transcript is torn — surfaced on the agent-error path, never a
        // clean result.
        if let Some(handle) = event_emitter
            && let Err(err) = handle.finish().await
        {
            return Err(super::driven::emitter_failure(&err));
        }

        let result = match result {
            Ok(value) => value,
            Err(err) => {
                return Err(err.into());
            }
        };

        // The diagnostic collector the builder wired onto the loop context
        // (via `load_runtime_base`) is the one runtime post-checks report
        // into; drain it for the output envelope. Absent only on a
        // library agent built without the runtime base — never here.
        let diagnostics = parts
            .loop_context
            .diagnostics
            .as_ref()
            .map(drain_diagnostics)
            .unwrap_or_default();
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
        checkpoint_session(&store).await?;
        let new_events = collect_new_events(&store, pre_event_count);

        let (output, usage) = extract_output_and_usage(&result);
        let stop_info = StopInfo::from_result(&result);
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
            session_id: output_session_id.as_deref(),
            events: &new_events,
            stop: &stop_info,
            diagnostics: &diagnostics,
        };
        // Driven mode returns the SAME structured result as the `-f json`
        // envelope, but as the id-matched `run/execute` response — the
        // single replay-authoritative output — instead of writing it to
        // stdout. This is the ONLY place the driven result is emitted, and
        // it is emitted as a Response (never a notification)
        // (`DRIVEN-PROTOCOL.md` "One-shot run lifecycle").
        if let Some(driver) = driven.as_ref() {
            let result_value = driven_result_value(&step)?;
            driver
                .finish_with_result(result_value)
                .map_err(|err| PrintError::Io(err.to_string()))?;
        } else {
            write_output(cli, format, &step)?;
        }
    }

    // NH-006 R8 / C61: SessionLifecycleHook::on_session_end fires on the
    // single normal-exit path. Errors return early above and skip this
    // hook by design — the brief's acceptance does not require firing
    // on panic, and explicit cleanup is preferred over a drop guard.
    parts.fire_session_end().await;

    Ok(final_exit_code)
}

/// Flush the store's persistence sink: pending durability work and the
/// sink's accumulated index delta land now instead of at drop. A no-op
/// for sink-less stores (`--no-session`).
///
/// Runs [`EventStore::checkpoint_off_executor`] — the blocking critical
/// section (inter-process index lock + full index rewrite + fsync)
/// What the orchestrator reports after honouring a pending `/clear`
/// (session-fidelity Gap 12).
#[derive(Debug)]
struct ClearReport {
    /// Session id every subsequent output surface (the `-f json`
    /// envelope, `write_handled_locally`, the driven `run/execute`
    /// response) must carry: the **live** slash-state cell, i.e. the
    /// rotated id after a rotation, the original id when no clear was
    /// pending, `None` under `--no-session`.
    envelope_session_id: Option<String>,
    /// Operator-facing stderr line, produced only AFTER the fallible
    /// rotation succeeded — so "Conversation cleared." can never precede
    /// a failed rotation — and naming the new session id on a rotation
    /// (parity with `/name`'s "Session named:" line).
    operator_line: Option<String>,
}

/// Apply a pending `/clear` and derive what to report.
///
/// The durability policy is the CLI's explicit `Flush` choice — the same
/// one the session front door passes in `from_cli` (D4).
///
/// # Errors
///
/// [`SessionPersistError`] from the rotation; the pre-clear slash state
/// is untouched on error (see `apply_clear_request`).
fn apply_clear_and_report(
    slash_state: &crate::commands::slash::SlashState,
) -> Result<ClearReport, SessionPersistError> {
    use crate::commands::slash::ClearOutcome;
    let operator_line =
        match apply_clear_request(slash_state, norn::session::DurabilityPolicy::Flush)? {
            Some(ClearOutcome::RotatedToNewSession { new_session_id }) => {
                tracing::debug!(
                    %new_session_id,
                    "conversation cleared via /clear in print mode; slash state \
                     rotated into a fresh persisted session",
                );
                Some(format!(
                    "Conversation cleared. New session: {new_session_id}"
                ))
            }
            Some(ClearOutcome::ClearedInMemory) => {
                tracing::debug!("conversation cleared via /clear in print mode (--no-session)");
                Some("Conversation cleared.".to_owned())
            }
            None => None,
        };
    Ok(ClearReport {
        envelope_session_id: slash_state.current_session_id(),
        operator_line,
    })
}

/// belongs on Tokio's blocking pool, never on the executor thread the
/// orchestrator's async path runs on.
async fn checkpoint_session(store: &Arc<EventStore>) -> Result<(), PrintError> {
    Arc::clone(store)
        .checkpoint_off_executor()
        .await
        .map_err(|err| PrintError::Session(err.to_string()))
}

/// Map a stream-renderer [`tokio::task::JoinError`] (panic or
/// cancellation) onto the agent-error path: the NDJSON already written to
/// stdout is incomplete, so the run must surface the failure on stderr
/// and exit non-zero instead of emitting a clean `completed` envelope.
/// Typed [`PrintError::StreamTorn`] so the error-envelope emitter skips
/// it (owner ruling R4): no terminal event is appended to a torn stream.
fn renderer_failure(err: &tokio::task::JoinError) -> PrintError {
    PrintError::StreamTorn(format!(
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    use crate::commands::slash::SlashState;
    use crate::commands::slash::state::SlashStateSeed;
    use crate::session::{CreateSessionOptions, SessionManager};

    /// A slash state as print mode seeds it, with `session_id`/`no_session`
    /// mirroring the invocation kind.
    fn slash_state_for(
        data_dir: &std::path::Path,
        session_id: Option<String>,
        no_session: bool,
    ) -> SlashState {
        SlashState::new(SlashStateSeed {
            model: "test-model".to_owned(),
            service_tier: None,
            reasoning_effort: None,
            output_schema: None,
            session_name: None,
            session_id,
            data_dir: data_dir.to_path_buf(),
            no_session,
            // Test configuration: generous bound, never contended here.
            index_lock_deadline: std::time::Duration::from_secs(10),
            variable_pairs: Vec::new(),
            tools: Vec::new(),
            store: Arc::new(EventStore::new()),
        })
    }

    /// Gap 12 operator surface: after a `/clear` rotation, the envelope
    /// session id and the stderr line both carry the ROTATED id — a
    /// driver resuming by the reported id must land on the post-clear
    /// timeline, never the retired session's full history.
    #[test]
    fn clear_report_carries_the_rotated_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        let opened = SessionManager::new(tmp.path())
            .create(
                CreateSessionOptions {
                    model: "test-model".to_owned(),
                    working_dir: "/work".to_owned(),
                    name: None,
                },
                norn::session::DurabilityPolicy::Flush,
            )
            .expect("create session");
        let old_id = opened.entry.id;
        let state = slash_state_for(tmp.path(), Some(old_id.clone()), false);
        state
            .clear_requested
            .store(true, std::sync::atomic::Ordering::Relaxed);

        let report = apply_clear_and_report(&state).expect("rotation succeeds");

        let new_id = report
            .envelope_session_id
            .expect("a persisted invocation keeps a session id");
        assert_ne!(new_id, old_id, "the envelope must name the NEW session");
        assert_eq!(
            state.current_session_id().as_deref(),
            Some(new_id.as_str()),
            "the envelope id is the live cell",
        );
        let line = report.operator_line.expect("a clear is reported");
        assert!(
            line.contains(&new_id),
            "the stderr line must name the rotated id: {line}",
        );
    }

    /// No pending `/clear`: nothing is printed and the envelope keeps the
    /// original session id.
    #[test]
    fn clear_report_without_pending_clear_keeps_the_original_id() {
        let tmp = tempfile::tempdir().unwrap();
        let state = slash_state_for(tmp.path(), Some("original-id".to_owned()), false);

        let report = apply_clear_and_report(&state).expect("no-op succeeds");

        assert!(report.operator_line.is_none());
        assert_eq!(report.envelope_session_id.as_deref(), Some("original-id"));
    }

    /// `--no-session` `/clear`: the sink-less choice propagates, the
    /// operator still gets the confirmation, and the envelope keeps
    /// reporting no session.
    #[test]
    fn clear_report_no_session_confirms_without_a_session_id() {
        let tmp = tempfile::tempdir().unwrap();
        let state = slash_state_for(tmp.path(), None, true);
        state
            .clear_requested
            .store(true, std::sync::atomic::Ordering::Relaxed);

        let report = apply_clear_and_report(&state).expect("in-memory clear succeeds");

        assert_eq!(
            report.operator_line.as_deref(),
            Some("Conversation cleared.")
        );
        assert!(report.envelope_session_id.is_none());
    }

    /// A failed rotation surfaces typed and reports NOTHING — the
    /// "Conversation cleared." line can never precede a failed rotation.
    #[test]
    fn clear_report_failure_prints_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        // A session id absent from the (empty) index: resolution fails.
        let state = slash_state_for(tmp.path(), Some("missing-session".to_owned()), false);
        state
            .clear_requested
            .store(true, std::sync::atomic::Ordering::Relaxed);

        let err = apply_clear_and_report(&state).expect_err("rotation must fail");
        assert!(matches!(err, SessionPersistError::NotFound { .. }));
        assert_eq!(
            state.current_session_id().as_deref(),
            Some("missing-session"),
            "the pre-clear state stays intact on error",
        );
    }

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
        assert_eq!(
            PrintError::StreamTorn("x".to_owned()).exit_code(),
            ExitCode::AgentError
        );
    }

    /// Owner rulings R2/R4: every failure class maps to its machine-stable
    /// envelope class, EXCEPT argument errors (clap parity, exit 2) and a
    /// torn stream (no envelope on incomplete NDJSON) — those emit nothing.
    #[test]
    fn envelope_class_covers_every_variant() {
        assert_eq!(PrintError::Argument("x".to_owned()).envelope_class(), None);
        assert_eq!(
            PrintError::StreamTorn("x".to_owned()).envelope_class(),
            None
        );
        assert_eq!(
            PrintError::Auth("x".to_owned()).envelope_class(),
            Some("auth")
        );
        assert_eq!(
            PrintError::Agent("x".to_owned()).envelope_class(),
            Some("agent")
        );
        assert_eq!(PrintError::Io("x".to_owned()).envelope_class(), Some("io"));
        assert_eq!(
            PrintError::Session("x".to_owned()).envelope_class(),
            Some("session")
        );
    }

    #[test]
    fn agent_step_result_exit_code_mapping() {
        use norn::provider::usage::Usage;

        let completed = AgentStepResult::Completed {
            output: serde_json::json!("done"),
            usage: Usage::default(),
            children_usage: Usage::default(),
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
            children_usage: Usage::default(),
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
            children_usage: Usage::default(),
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
            children_usage: Usage::default(),
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
            children_usage: Usage::default(),
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
            children_usage: Usage::default(),
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
            PrintError::StreamTorn(message) => {
                assert!(message.contains("panic"), "message: {message}");
                assert!(message.contains("incomplete"), "message: {message}");
            }
            other => panic!("expected StreamTorn, got {other:?}"),
        }
        assert_eq!(err.exit_code(), ExitCode::AgentError);
        assert_eq!(
            err.envelope_class(),
            None,
            "a torn stream never gets an error envelope (R4)"
        );
        // The stderr line is unchanged from the pre-StreamTorn rendering.
        assert!(err.to_string().starts_with("agent error: "));
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
            PrintError::StreamTorn(message) => {
                assert!(message.contains("cancelled"), "message: {message}");
            }
            other => panic!("expected StreamTorn, got {other:?}"),
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
            kind: norn::error::TransientKind::ConnectionReset,
        })
        .into();
        assert!(matches!(err, PrintError::Agent(_)));
    }
}
