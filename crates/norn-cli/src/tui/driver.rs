//! TUI driver — the `Cli` to `ExitCode` entry point for the norn binary.
//!
//! This module owns the runtime assembly (`build_runtime`, `build_provider`,
//! tool definitions, session, agent registry) and hands the
//! already-constructed objects to [`norn_tui::run_app`], which owns the
//! terminal setup and the event loop. The dependency direction is one
//! way: norn-cli depends on norn-tui, never the reverse, so the
//! `Cli` and `ExitCode` types stay on the CLI side.

use std::path::PathBuf;
use std::sync::Arc;

use norn::agent::registry::AgentRegistry;
use norn::agent_loop::runner::ToolExecutor;
use norn::provider::request::ToolDefinition;
use norn::session::store::{DurabilityPolicy, EventStore};
use norn::tools::lsp::{LspBackend, LspWorkspace, WorkspaceLspBackend};
use uuid::Uuid;

use norn_tui::TuiInputs;
use norn_tui::input::history::{InputHistory, default_history_path};
use norn_tui::render::fixed_panel::StatusBar;
use norn_tui::terminal::caps::TerminalCaps;

use crate::cli::{Cli, ExitCode};
use crate::print::build_provider;
use crate::runtime::{
    RuntimeBundle, RuntimeInputs, apply_system_prompt, build_runtime, install_agent_tool_infra,
    register_standard_tools,
};
use crate::session::{CreateSessionOptions, OpenSession, SessionManager};

/// Capacity of the agent-event broadcast channel shared by all agents.
const AGENT_EVENT_CHANNEL_CAPACITY: usize = 4096;

/// Synchronous entry point — matches the CLI dispatch pattern.
#[must_use]
pub fn run(cli: &Cli) -> ExitCode {
    if let Err(e) = TerminalCaps::check_hard_requirements() {
        eprintln!("{e}");
        return crate::print::run(cli);
    }

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

async fn run_async(cli: &Cli) -> ExitCode {
    match drive(cli).await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("norn: TUI error: {err}");
            ExitCode::AgentError
        }
    }
}

/// Assemble the runtime, register the root agent, and hand off to
/// `norn_tui::run_app`.
async fn drive(cli: &Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let mut inputs = RuntimeInputs::default();
    // LD-015 R3: construct ONE shared `LspWorkspace` at TUI startup so
    // the `lsp` tool, the `DiagnosticsPostCheck` LSP path, and the
    // `LspBridge` fast path all observe the same language-server
    // processes for the duration of the run. The workspace handle is
    // surfaced on `RuntimeInputs` so `build_runtime` can forward it
    // into `build_tool_context_with_diagnostics`.
    let lsp_workspace = Arc::new(LspWorkspace::with_builtins());
    let lsp_backend: Arc<dyn LspBackend> =
        Arc::new(WorkspaceLspBackend::new(Arc::clone(&lsp_workspace)));
    register_standard_tools(&mut inputs.registry, Some(Arc::clone(&lsp_backend)));
    inputs.lsp_workspace = Some(Arc::clone(&lsp_workspace));
    inputs.lsp_backend = Some(Arc::clone(&lsp_backend));
    let mut bundle = build_runtime(cli, inputs)?;
    apply_system_prompt(&mut bundle, norn::system_prompt::ExecutionMode::Interactive);

    // The working directory is recorded in the session index entry, so a
    // failed cwd read is a startup error — never silently degraded to an
    // empty string that poisons the index. Propagated as the session
    // stack's typed I/O error, matching the TUI `/new` path
    // (`norn_tui::app::slash::create_new_session_store`), which
    // propagates the same failure instead of defaulting.
    let working_dir = std::env::current_dir()
        .map_err(crate::session::SessionPersistError::from)?
        .to_string_lossy()
        .into_owned();

    let TuiSessionHandle {
        store,
        session_id,
        persist_data_dir,
        persist_session_id,
    } = open_session(cli, &bundle, working_dir)?;
    crate::runtime::install_action_log(&bundle.registry, &store, &mut bundle.loop_context);
    bundle.agent_config.cache_key = Some(session_id.clone());
    if let Some(env) = bundle.loop_context.environment.as_mut() {
        env.session_id = Some(session_id.clone());
    }
    if let Some(ref dir) = bundle.provider_overrides.debug_dump_dir {
        bundle.provider_overrides.debug_dump_file = Some(dir.join(format!("{session_id}.jsonl")));
    }

    let registry = AgentRegistry::shared();
    let root_id = register_root_agent(&registry, &bundle.model)?;
    let active_model = bundle.model.clone();
    let built_provider =
        build_provider(cli.provider, &bundle.provider_overrides, &active_model).await?;
    // The root agent has a single identity: the same `root_id` is used for
    // the registry entry (above), the `AgentToolInfra.agent_id` (here), and
    // the root `AgentEventSender` (below). Children spawned from the root
    // record `root_id` as their parent, so status-panel depth and the
    // session-tree hierarchy agree on who the root is.
    // The CLI's deliberate coordination envelope: child policy for every
    // spawn/fork plus the result-channel capacity, published alongside the
    // infra so spawn-time policy reads resolve (W3.2).
    let envelope = crate::runtime::cli_coordination_envelope();
    let child_result_capacity = envelope.child_result_capacity;
    install_agent_tool_infra(
        &bundle.registry,
        built_provider.as_arc(),
        Arc::clone(&store),
        root_id,
        Arc::clone(&bundle.registry),
        Arc::clone(&registry),
        envelope,
    );

    let (child_tx, child_rx) = tokio::sync::mpsc::channel::<
        norn::agent::result_channel::ChildAgentResult,
    >(child_result_capacity);
    let child_sender = norn::agent::result_channel::ChildResultSender(Arc::new(child_tx));
    crate::runtime::install_child_result_sender(&bundle.registry, child_sender);
    bundle.loop_context.child_result_rx = Some(child_rx);

    let tools = collect_tool_definitions(&bundle);
    let executor: Arc<dyn ToolExecutor> = Arc::clone(&bundle.registry) as Arc<dyn ToolExecutor>;
    let history = match default_history_path() {
        Some(path) => InputHistory::load_from(path),
        None => InputHistory::in_memory(),
    };
    let status_bar = StatusBar {
        model_name: bundle.model.clone(),
        session_name: session_id.clone(),
        input_tokens: 0,
        output_tokens: 0,
        key_hints: "^C exit".to_string(),
    };
    let initial_prompt = if cli.prompt.is_empty() {
        None
    } else {
        Some(cli.prompt.join(" "))
    };

    // NH-006 R8 / C60: fire SessionLifecycleHook::on_session_start now
    // that the runtime is fully assembled, before the TUI app launches.
    // Keep a clone of the Arc<HookRegistry> on the side so the matching
    // run_session_end fires after `norn_tui::run_app` returns — by then
    // the loop_context has been moved into the TUI inputs.
    let lifecycle_hooks: Option<std::sync::Arc<norn::integration::hooks::HookRegistry>> = bundle
        .loop_context
        .hooks
        .as_ref()
        .map(std::sync::Arc::clone);
    crate::runtime::wiring::run_session_start(lifecycle_hooks.as_ref(), &session_id).await;

    let (agent_event_tx, agent_event_rx) =
        tokio::sync::broadcast::channel::<norn::provider::AgentEvent>(AGENT_EVENT_CHANNEL_CAPACITY);
    let root_event_sender =
        norn::provider::AgentEventSender::new(agent_event_tx.clone(), root_id, "root".to_string());
    crate::runtime::install_shared_agent_event_channel(&bundle.registry, agent_event_tx);

    let tui_inputs = TuiInputs {
        provider: built_provider.as_arc(),
        executor,
        store: Arc::clone(&store),
        registry,
        loop_context: std::mem::take(&mut bundle.loop_context),
        agent_config: bundle.agent_config.clone(),
        model: bundle.model.clone(),
        tools,
        history,
        status_bar,
        root_id,
        initial_prompt,
        data_dir: persist_data_dir,
        session_id: persist_session_id,
        root_event_sender,
        agent_event_rx,
    };

    norn_tui::run_app(tui_inputs).await?;

    // NH-006 R8 / C61: fire SessionLifecycleHook::on_session_end on
    // normal-exit. Errors propagate via `?` above and skip this hook —
    // the brief's acceptance does not require firing on panic.
    crate::runtime::wiring::run_session_end(lifecycle_hooks.as_ref(), &session_id).await;

    Ok(ExitCode::Success)
}

/// Resolved session state for a TUI run.
///
/// `session_id` is always populated — under `--no-session` it is a fresh
/// UUID used only for the cache key, environment, and status bar, with no
/// on-disk record. `persist_data_dir` / `persist_session_id` are `Some`
/// only when the run persists events, and carry the directory and id the
/// TUI event loop appends to.
struct TuiSessionHandle {
    store: Arc<EventStore>,
    session_id: String,
    persist_data_dir: Option<PathBuf>,
    persist_session_id: Option<String>,
}

/// Resolve the session for a TUI run through [`SessionManager`],
/// honoring `--no-session`, `--resume`, `--fork`, and `--session-name`
/// exactly as the print orchestrator does — so the default interactive
/// mode is never the one entry point that ignores them.
///
/// - `--no-session`: a fresh in-memory [`EventStore`] with no sink and no
///   on-disk record.
/// - `--resume <id>`: replay the persisted session and continue
///   appending to the same file through its write-through sink.
/// - `--fork <id>`: copy the source session into a new one (with its
///   `Fork` marker).
/// - otherwise: create a fresh persisted session, honoring
///   `--session-name`.
///
/// Lines the tolerant reader skipped during a resume or fork replay are
/// reported on stderr (this runs before the terminal enters raw mode) —
/// a partial replay is never silent.
///
/// # Errors
///
/// Propagates [`crate::session::SessionPersistError`] when a resume / fork
/// source cannot be resolved or read, or when the new index entry cannot
/// be written.
fn open_session(
    cli: &Cli,
    bundle: &RuntimeBundle,
    working_dir: String,
) -> Result<TuiSessionHandle, Box<dyn std::error::Error>> {
    if cli.no_session {
        return Ok(TuiSessionHandle {
            store: Arc::new(EventStore::new()),
            session_id: Uuid::now_v7().to_string(),
            persist_data_dir: None,
            persist_session_id: None,
        });
    }

    let data_dir = crate::config::session_data_dir();
    let manager = SessionManager::new(&data_dir);

    let opened = if let Some(id) = cli.resume.as_deref() {
        manager.resume(id, DurabilityPolicy::Flush)?
    } else if let Some(id) = cli.fork.as_deref() {
        manager.fork(
            id,
            CreateSessionOptions {
                model: bundle.model.clone(),
                working_dir,
                name: None,
            },
            DurabilityPolicy::Flush,
        )?
    } else {
        manager.create(
            CreateSessionOptions {
                model: bundle.model.clone(),
                working_dir,
                name: cli.session_name.clone(),
            },
            DurabilityPolicy::Flush,
        )?
    };
    warn_if_lines_skipped(&opened);
    Ok(TuiSessionHandle {
        store: Arc::new(opened.store),
        session_id: opened.entry.id.clone(),
        persist_data_dir: Some(data_dir),
        persist_session_id: Some(opened.entry.id),
    })
}

/// Surface a partial replay on stderr before the TUI takes over the
/// terminal: the tolerant reader skips torn, corrupt, unknown, and
/// duplicate lines instead of failing the load, and that count must
/// reach the user.
fn warn_if_lines_skipped(opened: &OpenSession) {
    if opened.replay.skipped_lines > 0 {
        eprintln!(
            "norn: warning: {} corrupt or unreadable line(s) skipped while loading session {}",
            opened.replay.skipped_lines, opened.entry.id,
        );
    }
}

/// Register the root agent in the shared registry so the TUI's
/// [`norn_tui::agents::status_line::AgentStatusPanel`] has a known root
/// id to compute the depth-0 entry against.
fn register_root_agent(
    registry: &Arc<parking_lot::RwLock<AgentRegistry>>,
    model: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let guard = AgentRegistry::reserve(
        registry,
        "/root".to_string(),
        "lead".to_string(),
        model.to_string(),
        None,
    )?;
    let id = guard.id();
    guard.confirm()?;
    Ok(id)
}

fn collect_tool_definitions(bundle: &RuntimeBundle) -> Vec<ToolDefinition> {
    norn::provider::collect_function_definitions(&bundle.registry, None)
}
