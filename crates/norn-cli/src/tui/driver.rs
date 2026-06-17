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
    install_pending_agent_messages_for_loop, register_standard_tools,
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
    match Box::pin(drive(cli)).await {
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
    let built_provider = build_provider(
        bundle.provider_kind,
        &bundle.provider_overrides,
        &active_model,
    )
    .await?;
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
    // W3.7 root inbound wiring: the returned receiver is the root's
    // inbound channel — its sender is registered in the MessageRouter
    // under `root_id`, so a child's `signal_agent(to: "parent")` lands
    // here. It is handed to the TUI event loop (TuiInputs.root_inbound),
    // which threads it into every root step; the route is
    // process-lifetime and survives `/new` rotation untouched, because
    // rotation reuses both the router and the root identity
    // (norn-tui's `rotate_store_dependents`).
    let root_inbound = install_agent_tool_infra(
        &bundle.registry,
        built_provider.as_arc(),
        Arc::clone(&store),
        root_id,
        Arc::clone(&bundle.registry),
        Arc::clone(&registry),
        envelope,
    );
    install_pending_agent_messages_for_loop(&bundle.registry, &mut bundle.loop_context);

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
        service_tier: bundle
            .loop_context
            .service_tier
            .map(|tier| tier.as_str().to_string()),
        reasoning_effort: bundle
            .loop_context
            .reasoning_effort
            .map(|effort| effort.as_str().to_string()),
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
        root_inbound,
    };

    let app_result = norn_tui::run_app(tui_inputs).await;

    // NH-006 R8 / C61: fire SessionLifecycleHook::on_session_end after
    // the TUI returns, including terminal/runtime errors. The hook is
    // observational; preserve the original TUI result after it runs.
    crate::runtime::wiring::run_session_end(lifecycle_hooks.as_ref(), &session_id).await;
    app_result?;

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
///   appending to the same file through its write-through sink. With no
///   argument, resumes the most recently updated session for the current
///   working directory.
/// - `--fork <id>`: copy the source session into a new one (with its
///   `Fork` marker). With no argument, forks the most recently updated
///   session for the current working directory.
/// - `--session-id <id>`: create a fresh persisted session under the
///   caller's exact ID — a typed failure when the ID already exists
///   unless `--resume-if-exists` is also supplied
///   (create-exactly-this, same semantics as print mode; clap rejects
///   combining it with `--resume`/`--fork`/`--no-session`). Honors
///   `--session-name` only on the create arm.
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
        if id.trim().is_empty() {
            manager.resume_latest_in_working_dir(&working_dir, DurabilityPolicy::Flush)?
        } else {
            manager.resume(id, DurabilityPolicy::Flush)?
        }
    } else if let Some(id) = cli.fork.as_deref() {
        let options = CreateSessionOptions {
            model: bundle.model.clone(),
            working_dir: working_dir.clone(),
            name: None,
        };
        if id.trim().is_empty() {
            manager.fork_latest_in_working_dir(&working_dir, options, DurabilityPolicy::Flush)?
        } else {
            manager.fork(id, options, DurabilityPolicy::Flush)?
        }
    } else if let Some(id) = cli.session_id.as_deref() {
        let options = CreateSessionOptions {
            model: bundle.model.clone(),
            working_dir,
            name: cli.session_name.clone(),
        };
        if cli.resume_if_exists {
            manager.open_or_resume(id, options, DurabilityPolicy::Flush)?
        } else {
            manager.create_with_id(id, options, DurabilityPolicy::Flush)?
        }
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
    // The root entry carries the CLI envelope's child policy — the root's
    // own granted budget, the ground truth its spawn/fork reservations are
    // checked against (W3.4).
    let guard = AgentRegistry::reserve(
        registry,
        "/root".to_string(),
        "lead".to_string(),
        model.to_string(),
        None,
        crate::runtime::cli_coordination_envelope().child_policy,
        None,
    )?;
    let id = guard.id();
    guard.confirm()?;
    Ok(id)
}

fn collect_tool_definitions(bundle: &RuntimeBundle) -> Vec<ToolDefinition> {
    norn::provider::collect_function_definitions(&bundle.registry, None)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
mod tests {
    use clap::Parser;

    use super::*;

    struct TempNornHome {
        prior: Option<std::ffi::OsString>,
        _tempdir: tempfile::TempDir,
    }

    impl TempNornHome {
        fn new(tempdir: tempfile::TempDir) -> Self {
            let prior = std::env::var_os("NORN_HOME");
            // SAFETY: paired with the `#[serial]` marker on every consumer;
            // no concurrent reader observes the mutated env.
            unsafe { std::env::set_var("NORN_HOME", tempdir.path()) };
            Self {
                prior,
                _tempdir: tempdir,
            }
        }
    }

    impl Drop for TempNornHome {
        fn drop(&mut self) {
            match &self.prior {
                Some(val) => unsafe { std::env::set_var("NORN_HOME", val) },
                None => unsafe { std::env::remove_var("NORN_HOME") },
            }
        }
    }

    #[test]
    #[serial_test::serial]
    fn empty_resume_selects_latest_session_for_tui_working_dir() {
        let _home = TempNornHome::new(tempfile::tempdir().unwrap());
        let create_cli = Cli::try_parse_from(["norn"]).unwrap();
        let bundle = build_runtime(&create_cli, RuntimeInputs::default()).unwrap();
        let current_session = open_session(&create_cli, &bundle, "/repo/current".to_owned())
            .expect("current-dir session created");
        let current_id = current_session.session_id.clone();
        drop(current_session);

        std::thread::sleep(std::time::Duration::from_millis(5));
        let other_session = open_session(&create_cli, &bundle, "/repo/other".to_owned())
            .expect("other-dir session created");
        let other_id = other_session.session_id.clone();
        drop(other_session);

        let resume_cli = Cli::try_parse_from(["norn", "--resume"]).unwrap();
        let resumed = open_session(&resume_cli, &bundle, "/repo/current".to_owned())
            .expect("current-dir session resumed");
        assert_eq!(
            resumed.session_id, current_id,
            "must not resume globally newer other-dir session {other_id}",
        );
    }
}
