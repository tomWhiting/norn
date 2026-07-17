//! TUI driver — the `Cli` to `ExitCode` entry point for the norn binary.
//!
//! This module resolves the CLI invocation, builds the provider, and
//! assembles the agent through the library `AgentBuilder`
//! (`builder_from_cli` → `into_parts`), then maps the resulting
//! `AgentParts` onto [`norn_tui::run_app`], which owns the
//! terminal setup and the event loop. The dependency direction is one
//! way: norn-cli depends on norn-tui, never the reverse, so the
//! `Cli` and `ExitCode` types stay on the CLI side.

use std::sync::Arc;

use norn::agent::registry::AgentRegistry;
use norn::system_prompt::ExecutionMode;
use norn::tools::lsp::{LspBackend, WorkspaceLspBackend, build_lsp_workspace};

use norn_tui::TuiInputs;
use norn_tui::input::history::{InputHistory, default_history_path};
use norn_tui::render::fixed_panel::StatusBar;
use norn_tui::terminal::caps::TerminalCaps;

use crate::cli::{Cli, ExitCode};
use crate::print::build_provider;
use crate::runtime::{
    builder_from_cli, cli_coordination_envelope, connect_mcp_runtime, resolve_invocation,
    warn_unmatched_tool_flag_names,
};

use super::startup_trace::StartupTrace;

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

/// Assemble the interactive TUI agent through the single library-owned
/// assembler and hand its [`AgentParts`](norn::agent::AgentParts) to
/// `norn_tui::run_app`.
///
/// The builder opens the session (`.open_session`), installs the action
/// log, stamps the cache key / environment session id, registers the
/// depth-0 root in the shared [`AgentRegistry`] (`.register_root`), wires
/// the agent-coordination infra, and creates the event broadcast +
/// inbound channels. Terminal reclamation is **`false`**: the TUI's agent
/// status panel owns reclamation through its hold window, so the builder
/// must not install the headless reclamation marker.
///
/// One [`AgentRegistry`] `Arc` clone is kept for `TuiInputs.registry` (the
/// status panel reads it); the other is handed to the builder.
async fn drive(cli: &Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let mut startup_trace = StartupTrace::start();
    let oauth_account = cli.account.as_deref();

    // LD-015 R3: construct ONE shared `LspWorkspace` at TUI startup so the
    // `lsp` tool, the `DiagnosticsPostCheck` LSP path, and the `LspBridge`
    // fast path all observe the same language-server processes for the
    // duration of the run. The builder forwards both handles into the
    // diagnostics post-check pipeline.
    let lsp_workspace = build_lsp_workspace()?;
    let lsp_backend: Arc<dyn LspBackend> =
        Arc::new(WorkspaceLspBackend::new(Arc::clone(&lsp_workspace))?);
    startup_trace.mark("lsp_workspace_ready");

    let resolved = resolve_invocation(cli)?;
    startup_trace.mark("invocation_resolved");

    // Debug-dump file naming: the provider is built before the session id
    // is minted, so the dump is named from the only pre-resolvable
    // identifier (`--session-id`, else `--session-name`, else `unnamed`).
    let mut provider_overrides = resolved.provider_overrides;
    if let Some(dir) = provider_overrides.debug_dump_dir.clone() {
        let hint = cli
            .session_id
            .as_deref()
            .or(cli.session_name.as_deref())
            .unwrap_or("unnamed");
        norn::util::validate_private_component(hint, "debug dump session name")?;
        provider_overrides.debug_dump_file = Some(dir.join(format!("{hint}.jsonl")));
    }

    let built_provider = build_provider(
        resolved.provider_kind,
        &provider_overrides,
        &resolved.model,
        oauth_account,
        cli.agent_run_may_reuse_session(),
    )
    .await?;
    startup_trace.mark("provider_built");

    let mcp = connect_mcp_runtime(&resolved.project_root, &resolved.mcp_servers).await?;
    for server in &mcp.pending_project_servers {
        eprintln!(
            "norn: MCP server '{server}' is waiting for shared-project approval; from {} run `norn mcp approve {server}`",
            resolved.project_root.display(),
        );
    }
    for (server, error) in &mcp.failed_servers {
        eprintln!("norn: MCP server '{server}' is unavailable; continuing without it: {error}");
    }
    if let Some(error) = mcp.project_approval_error.as_deref() {
        eprintln!("norn: project MCP approvals could not be read: {error}");
    }
    startup_trace.mark("mcp_ready");

    // The status-panel registry: one clone stays with the TUI event loop
    // (`TuiInputs.registry`), the other is handed to the builder so the
    // registered root and every spawned child share one registry.
    let registry = AgentRegistry::shared();
    let envelope = cli_coordination_envelope(resolved.delegation_depth);

    let mut builder = builder_from_cli(
        cli,
        built_provider.as_arc(),
        resolved.profile,
        &resolved.settings,
        &resolved.applied,
    )?;
    if let Some(runtime) = mcp.runtime {
        if let Some(servers) = resolved
            .settings
            .agent
            .as_ref()
            .and_then(|agent| agent.mcp_servers.as_deref())
        {
            builder = builder.mcp_runtime_for_servers(runtime, servers)?;
        } else {
            builder = builder.mcp_runtime(runtime);
        }
    }
    builder = builder.mcp_config_state(resolved.mcp_state);
    let agent = builder
        .execution_mode(ExecutionMode::Interactive)
        .lsp_workspace(Arc::clone(&lsp_workspace))
        .lsp_backend(Arc::clone(&lsp_backend))
        .agent_registry(Arc::clone(&registry))
        .child_policy(envelope.child_policy.clone())
        .child_result_capacity(envelope.child_result_capacity)
        .event_channel_capacity(AGENT_EVENT_CHANNEL_CAPACITY)
        .inbound_capacity(envelope.child_policy.inbound_capacity)
        .register_root("/root".to_string(), "lead".to_string())
        .terminal_reclamation(false)
        .build()?;
    startup_trace.mark("runtime_built");

    let mut parts = agent.into_parts();

    // Warn on `--allowed-tools` / `--disallowed-tools` names that match no
    // real tool: the gated registry is only known after `build()`.
    warn_unmatched_tool_flag_names(&parts.registry, &resolved.applied);

    // `info.session_id` is always populated (fresh UUID under
    // `--no-session`); `session_entry` is `Some` only for a persisted
    // session, carrying the id and directory the TUI event loop appends to.
    let session_id = parts.info.session_id.clone();
    let persist_session_id = parts.session_entry.as_ref().map(|entry| entry.id.clone());
    let persist_data_dir = parts
        .session_entry
        .as_ref()
        .map(|_| crate::config::session_data_dir())
        .transpose()?;
    startup_trace.mark_session(
        "session_opened",
        &session_id,
        parts.event_store.len(),
        persist_session_id.is_some(),
    );

    let root_id = parts.id;
    let history = match default_history_path() {
        Some(path) => InputHistory::load_from(&path),
        None => InputHistory::in_memory(),
    };
    startup_trace.mark_count("input_history_loaded", "entries", history.len());
    let status_bar = StatusBar {
        model_name: parts.model.clone(),
        session_name: session_id.clone(),
        input_tokens: 0,
        input_tokens_estimated: false,
        output_tokens: 0,
        output_tokens_estimated: false,
        key_hints: "^C exit".to_string(),
        service_tier: parts
            .loop_context
            .service_tier
            .map(|tier| tier.as_str().to_string()),
        reasoning_effort: parts
            .loop_context
            .reasoning_effort
            .map(|effort| effort.as_str().to_string()),
    };
    let initial_prompt = if cli.prompt.is_empty() {
        None
    } else {
        Some(cli.prompt.join(" "))
    };

    // Session-lifecycle hooks (D1 / R1.7): the TUI runs its own multi-turn
    // loop (never `Agent::run`), so it fires the hooks explicitly around
    // `run_app`. The registry clone is retained because `loop_context` is
    // moved into `TuiInputs` below; the end hook fires after `run_app`
    // returns with the resolved (non-empty) session id.
    let session_hooks = parts.loop_context.hooks.clone();
    if let Some(hooks) = session_hooks.as_ref() {
        hooks.run_session_start(&session_id).await;
    }
    startup_trace.mark("session_start_hooks_ran");

    let Some(agent_event_tx) = parts.events_tx.clone() else {
        return Err("event broadcast channel missing after assembly \
             (event_channel_capacity was not wired)"
            .into());
    };
    let agent_event_rx = agent_event_tx.subscribe();
    let Some(root_event_sender) = parts.event_sender.clone() else {
        return Err("root event sender missing after assembly \
             (event_channel_capacity was not wired)"
            .into());
    };

    // The live tool runtime supplies an immutable generation lease per
    // provider request. Passing the Arc as a trait object also preserves the
    // owned executor handles used by concurrent tool batches.
    let executor: Arc<dyn norn::agent_loop::config::ToolExecutor> =
        Arc::clone(&parts.tool_runtime) as Arc<dyn norn::agent_loop::config::ToolExecutor>;
    let tui_inputs = TuiInputs {
        provider: Arc::clone(&parts.provider),
        executor,
        store: Arc::clone(&parts.event_store),
        registry,
        agent_config: parts.config.clone(),
        model: parts.model.clone(),
        tools: std::mem::take(&mut parts.tool_defs),
        loop_context: std::mem::take(&mut parts.loop_context),
        history,
        status_bar,
        root_id,
        initial_prompt,
        data_dir: persist_data_dir,
        session_id: persist_session_id,
        // Bounds the `/new` rotation's index-lock wait — resolved from
        // settings / `-c index_lock_deadline_ms` alongside the rest of
        // the invocation, same value `builder_from_cli` applied.
        index_lock_deadline: resolved.index_lock_deadline,
        root_event_sender,
        agent_event_rx,
        root_inbound: parts.inbound.take(),
        mcp_control: parts.mcp_control.take(),
    };
    startup_trace.mark("handoff_to_tui_app");

    let app_result = norn_tui::run_app(tui_inputs).await;

    // NH-006 R8 / C61: fire on_session_end after the TUI returns, including
    // terminal/runtime errors. The hook is observational; preserve the
    // original TUI result after it runs.
    if let Some(hooks) = session_hooks.as_ref() {
        hooks.run_session_end(&session_id).await;
    }
    app_result?;

    Ok(ExitCode::Success)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use norn::agent::AgentBuilder;
    use norn::provider::mock::MockProvider;
    use norn::provider::traits::Provider;

    use super::*;

    fn mock_provider() -> Arc<dyn Provider> {
        Arc::new(MockProvider::new(Vec::new()))
    }

    /// Explicit window for tests whose model id is deliberately
    /// uncatalogued, and `build` now hard-errors on an unarmed window
    /// (2026-07-05 incident guard). `272_000` is gpt-5.5's catalogued
    /// standard window (assets/models.json) — factual, not invented.
    /// Same constant the libnorn builder/instance tests use.
    const TEST_CONTEXT_WINDOW: u64 = 272_000;

    /// R1.6: the TUI's coordination chain — `.agent_registry` +
    /// `.event_channel_capacity` + `.inbound_capacity` + `.register_root` +
    /// `.terminal_reclamation(false)` — produces `AgentParts` carrying the
    /// root inbound channel (so child `signal_agent(to: "parent")` drains
    /// into root steps) and the event broadcast channel (so the status
    /// panel and stream renderer subscribe to one channel). This is the
    /// exact chain `drive` builds; the mock provider stands in for the
    /// concrete backend `build_provider` would return.
    #[tokio::test]
    async fn tui_parts_carry_root_inbound_and_event_channel() {
        let envelope = cli_coordination_envelope(crate::runtime::DEFAULT_DELEGATION_DEPTH);
        let agent = AgentBuilder::new(mock_provider())
            .model("test-model")
            .context_window_limit(TEST_CONTEXT_WINDOW)
            .working_dir(std::env::temp_dir())
            .execution_mode(ExecutionMode::Interactive)
            .agent_registry(AgentRegistry::shared())
            .child_policy(envelope.child_policy.clone())
            .child_result_capacity(envelope.child_result_capacity)
            .event_channel_capacity(AGENT_EVENT_CHANNEL_CAPACITY)
            .inbound_capacity(envelope.child_policy.inbound_capacity)
            .register_root("/root".to_string(), "lead".to_string())
            .terminal_reclamation(false)
            .build()
            .expect("build succeeds");
        let parts = agent.into_parts();
        assert!(
            parts.inbound.is_some(),
            "the TUI root must carry an inbound channel for child->root steering",
        );
        assert!(
            parts.events_tx.is_some(),
            "the TUI must carry the event broadcast channel",
        );
        assert!(
            parts.event_sender.is_some(),
            "the root event sender is present alongside the broadcast channel",
        );
        assert!(
            parts.loop_context.child_result_rx.is_some(),
            "the child-result receiver must be wired for spawn/fork completion",
        );
    }
}
