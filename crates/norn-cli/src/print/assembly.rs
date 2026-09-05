//! Headless agent assembly shared by print and driven execution.

use norn::agent::AgentParts;
use norn::agent::registry::AgentRegistry;
use norn::system_prompt::ExecutionMode;
use norn::tools::lsp::build_lsp_backend;

use super::error::PrintError;
use super::provider::build_provider;
use crate::cli::{Cli, ExitCode, Mode};
use crate::runtime::{
    builder_from_cli, cli_coordination_envelope, initialize_cli_channels, prepare_cli_mcp,
    resolve_invocation, warn_unmatched_runtime_tool_flag_names,
};

/// Buffer size for the streaming-event broadcast channel the builder
/// creates via `.event_channel_capacity`. Sized so a brief burst of
/// provider events does not push a slow consumer into `Lagged`.
const BROADCAST_BUFFER_CAPACITY: usize = 256;

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
    let oauth_account = cli.account.as_deref();
    let resolved = resolve_invocation(cli)?;
    crate::runtime::channel_config::validate_resolved_channel_mode(
        resolved.channel_config.as_ref(),
        cli.protocol,
        Mode::Print,
    )?;
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

    let built_provider = build_provider(
        resolved.provider_kind,
        &provider_overrides,
        &resolved.model,
        oauth_account,
    )
    .await
    .map_err(|err| match err.exit_code() {
        ExitCode::AuthError => PrintError::Auth(err.to_string()),
        _ => PrintError::Agent(err.to_string()),
    })?;

    let mcp = prepare_cli_mcp(
        &resolved.project_root,
        &resolved.mcp_servers,
        resolved.channel_config.as_ref(),
    )
    .await
    .map_err(|error| PrintError::Agent(error.to_string()))?;
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

    let envelope = cli_coordination_envelope(resolved.delegation_depth);
    let mut builder = builder_from_cli(
        cli,
        built_provider.as_arc(),
        resolved.profile,
        resolved.profile_source,
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
            builder = builder
                .mcp_runtime_for_servers(runtime, servers)
                .map_err(|error| PrintError::Agent(error.to_string()))?;
        } else {
            builder = builder.mcp_runtime(runtime);
        }
    }
    builder = builder.mcp_config_state(resolved.mcp_state);
    if let Some(channels) = resolved.channel_config.as_ref() {
        builder = builder.mcp_channels(channels.clone());
        if let Some(servers) = resolved
            .settings
            .agent
            .as_ref()
            .and_then(|agent| agent.mcp_servers.as_ref())
        {
            builder = builder.mcp_selected_servers(servers.clone());
        }
    }
    let agent = builder
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
    let mut parts = agent.into_parts();
    parts
        .model_selection
        .bind_provider_profile(resolved.provider_profile);
    initialize_cli_channels(&mut parts, resolved.channel_config.as_ref())
        .await
        .map_err(|error| PrintError::Agent(error.to_string()))?;
    // Deferred until here (not inside `builder_from_cli`) because gating
    // happens during `build()`: the assembled registry is the authoritative
    // reference for which flag-named tools exist.
    warn_unmatched_runtime_tool_flag_names(&parts, &resolved.applied);
    Ok(PrintAssembly {
        parts,
        index_lock_deadline,
    })
}
