//! Runtime wiring helpers added by NC-003 R3 and extended by NC-009.
//!
//! [`crate::runtime::build_runtime`] already threads
//! [`norn::agent_loop::tokens::SimpleTokenEstimator`],
//! [`norn::session::context_edit::ContextEdits`], retry policy, and event
//! schemas onto the [`norn::agent_loop::loop_context::LoopContext`]. This
//! module fills the remaining gaps:
//!
//! 1. [`DiagnosticCollector`] construction — produced as an `Arc` and
//!    carried on [`crate::runtime::bundle::RuntimeBundle`] for draining after
//!    `run_agent_step`. The same `Arc` is wired onto
//!    [`norn::agent_loop::loop_context::LoopContext::diagnostics`] and
//!    published on the [`norn::tool::registry::ToolRegistry`]'s shared
//!    [`norn::tool::context::ToolContext`] via `insert_extension`
//!    (NC-009 R1) so runtime post-validate checks and tool implementations
//!    report into the same sink the CLI drains.
//! 2. [`iteration_monitor_from_profile`] — parses the optional
//!    `[iteration_monitor]` section out of [`norn::profile::Profile::settings`]
//!    via a CLI-side serde-derived mirror struct, since libnorn's
//!    [`IterationMonitorConfig`] is not Serde-derived.
//! 3. [`length_limit_from_profile`] — parses the optional
//!    `[tool_config.write]` section into a [`LengthLimit`] for
//!    [`WriteTool::with_length_limit`], applies a CLI `-c
//!    write.max_code_lines=N` override that takes precedence over the
//!    profile value, and returns [`LengthLimit::none`] when neither is
//!    configured (NC-009 R2 / R3).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;

use norn::agent::child_policy::{
    ChildPolicy, CoordinationEnvelope, DelegationBudget, MessagingScope,
};
use norn::agent::message_router::MessageRouter;
use norn::agent::registry::AgentRegistry;
use norn::agent::result_channel::ChildResultSender;
use norn::config::NornSettings;
use norn::integration::DiagnosticCollector;
use norn::integration::hooks::HookRegistry;

use norn::agent_loop::commands::SlashCommandRegistry;
use norn::agent_loop::iteration::IterationMonitorConfig;
use norn::agent_loop::runner::ToolExecutor;
use norn::profile::Profile;

use norn::internal::extraction::SharedProvider;
use norn::provider::traits::Provider;
use norn::session::store::EventStore;
use norn::skill::SkillCatalog;
use norn::tool::registry::ToolRegistry;
use norn::tools::agent::AgentToolInfra;
use norn::tools::write::{LengthLimit, WriteTool};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::bundle::RuntimeBundle;

use crate::cli::Cli;

use crate::commands::slash::state::SlashStateSeed;

use crate::commands::slash::{SlashState, build_slash_registry};

use crate::config::parse_kv;

use crate::cli::BuildError;
use crate::config::ConfigOverrides;

use crate::config::parse_inline_or_file;

use crate::config::session_data_dir;

#[cfg(test)]
mod inline_tests;
#[cfg(test)]
mod wiring_tests;

/// The CLI's deliberate [`CoordinationEnvelope`]: the child policy and
/// channel capacities every agent spawned from a CLI-assembled runtime
/// runs under.
///
/// The values are the Wave 3 design's documented proposals — a conscious
/// per-deployment choice by the CLI, never a library default (the library
/// requires every embedder to supply its own envelope):
///
/// - `messaging: SiblingsAndParent` — the audit trail and the steer/update
///   split are the safety mechanism, not isolation (DECISION M1).
/// - `remaining_depth: 1`, `max_concurrent_children: 32` — today's
///   production-proven delegation shape; deeper trees are an explicit
///   opt-in per deployment (DECISION R1).
/// - `inbound_capacity: 32` — the per-child inbound backpressure buffer
///   (DECISION M4).
/// - `child_result_capacity: 256` — the child-result channel buffer
///   (DECISION R3); the CLI's root result channel is sized from this same
///   value so the two cannot drift.
#[must_use]
pub fn cli_coordination_envelope() -> CoordinationEnvelope {
    CoordinationEnvelope {
        child_policy: ChildPolicy {
            messaging: MessagingScope::SiblingsAndParent,
            delegation: DelegationBudget {
                remaining_depth: 1,
                max_concurrent_children: 32,
            },
            inbound_capacity: 32,
        },
        child_result_capacity: 256,
    }
}

/// Register the standard tool set into a [`ToolRegistry`].
///
/// Install [`AgentToolInfra`] on the registry's shared
/// [`norn::tool::context::ToolContext`] so the four agent-coordination
/// tools (`spawn_agent`, `fork`, `send_message`,
/// `close_agent`) resolve their runtime infrastructure instead of
/// erroring with a typed `MissingExtension` error naming
/// `AgentToolInfra`, and publish the CLI's deliberate
/// [`CoordinationEnvelope`] (`envelope`) so the spawn/fork launch paths
/// can read the child policy — without it every spawn/fork fails with a
/// typed `MissingExtension` naming the envelope.
///
/// `build_runtime` is synchronous and runs before the provider is
/// constructed, so this step is split out: callers invoke it after
/// `build_provider` succeeds, passing the shared `Arc<dyn Provider>`
/// alongside the session's [`EventStore`] and the root agent's id.
///
/// `tool_registry` is the same `Arc<ToolRegistry>` that the root agent
/// dispatches through. Spawned sub-agents look up `Tool` implementations
/// from this registry and dispatch them against their own per-child
/// `ToolContext` (the Path 3 identity fix).
///
/// The CLI's root agent runs without an inbound channel, so no root route
/// is registered in the [`MessageRouter`]: a child messaging `"parent"`
/// receives the honest `NotRouted` failure rather than queueing into a
/// channel nothing drains. Wiring a root inbound channel for the CLI
/// drivers is a separate, deliberate feature.
pub fn install_agent_tool_infra(
    registry: &ToolRegistry,
    provider: Arc<dyn Provider>,
    event_store: Arc<EventStore>,
    agent_id: Uuid,
    tool_registry: Arc<ToolRegistry>,
    agent_registry: Arc<RwLock<AgentRegistry>>,
    envelope: CoordinationEnvelope,
) {
    let Some(shared) = registry.shared_context() else {
        return;
    };
    shared.insert_extension(Arc::new(SharedProvider(Arc::clone(&provider))));
    shared.insert_extension(Arc::new(envelope));
    let infra = AgentToolInfra {
        registry: agent_registry,
        router: Arc::new(MessageRouter::new()),
        provider,
        event_store,
        agent_id,
        parent_id: None,
        grant: None,
        tool_registry: Some(tool_registry),
    };
    shared.insert_extension(Arc::new(infra));
}

/// Install a [`ChildResultSender`] on the registry's shared
/// [`norn::tool::context::ToolContext`] so fork and spawn tools can
/// send completion results to the orchestrator's outer loop.
pub fn install_child_result_sender(registry: &ToolRegistry, sender: ChildResultSender) {
    let Some(shared) = registry.shared_context() else {
        return;
    };
    shared.insert_extension(Arc::new(sender));
}

/// Declare delivery-anchored reclamation of finished children on the
/// registry's shared [`norn::tool::context::ToolContext`] — the
/// **headless** (print / non-TUI) driver's half of the reclamation
/// ownership split.
///
/// Installs the [`norn::tools::agent::ReclaimOnResultDelivery`] marker
/// via [`norn::runtime_init::install_terminal_reclamation`]: once a
/// spawned or forked child's result has been delivered through the
/// child-result channel, the launch wrapper reclaims the child's
/// terminal registry entry and the parent-held handle, so headless runs
/// do not pin one event store per finished child.
///
/// The TUI driver must **not** call this — its agent status panel
/// displays terminal entries through a hold window and reclaims them
/// itself; installing the marker there would race the hold window into
/// nonexistence. `build_runtime` is shared between both drivers, which
/// is why this lives in per-driver wiring instead.
pub fn install_headless_reclamation(registry: &ToolRegistry) {
    let Some(shared) = registry.shared_context() else {
        return;
    };
    norn::runtime_init::install_terminal_reclamation(&shared);
}

/// Install the shared agent event broadcast channel on the tool
/// registry's shared context so fork/spawn tools can create child
/// [`AgentEventSender`](norn::provider::AgentEventSender) instances.
///
/// The extension holds an **owned** `Sender` clone, so the broadcast
/// channel never closes while the registry's shared context is alive
/// (REVIEW C1). Consumers must therefore not await channel closure to
/// detect end-of-stream — the print renderer uses the explicit
/// [`StreamRendererHandle::finish`](crate::print::StreamRendererHandle::finish)
/// shutdown signal instead.
pub fn install_shared_agent_event_channel(
    registry: &ToolRegistry,
    tx: tokio::sync::broadcast::Sender<norn::provider::AgentEvent>,
) {
    let Some(shared) = registry.shared_context() else {
        return;
    };
    shared.insert_extension(Arc::new(norn::provider::SharedAgentEventChannel(tx)));
}

/// NH-006 R8 / C60: fire every registered
/// [`SessionLifecycleHook::on_session_start`](norn::integration::hooks::SessionLifecycleHook::on_session_start)
/// at the moment the CLI driver opens a session, before the first
/// `run_agent_step` call. `hooks` is the same `Arc<HookRegistry>` that
/// [`crate::runtime::build_runtime`] installs on
/// [`norn::agent_loop::loop_context::LoopContext::hooks`]; passing `None`
/// is a no-op so the call site can invoke this unconditionally.
pub async fn run_session_start(hooks: Option<&Arc<HookRegistry>>, session_id: &str) {
    if let Some(h) = hooks {
        h.run_session_start(session_id).await;
    }
}

/// NH-006 R8 / C61: counterpart to [`run_session_start`] — fires every
/// registered
/// [`SessionLifecycleHook::on_session_end`](norn::integration::hooks::SessionLifecycleHook::on_session_end)
/// at session teardown (the driver's explicit cleanup path, since a
/// drop guard cannot `.await`). Observational; the return value of any
/// hook is ignored.
pub async fn run_session_end(hooks: Option<&Arc<HookRegistry>>, session_id: &str) {
    if let Some(h) = hooks {
        h.run_session_end(session_id).await;
    }
}

/// Construct a fresh [`DiagnosticCollector`] shared via `Arc`.
///
/// Returned as an `Arc` so the CLI can publish the same collector onto
/// [`crate::runtime::bundle::RuntimeBundle`], onto
/// [`norn::agent_loop::loop_context::LoopContext::diagnostics`], and onto
/// the [`norn::tool::context::ToolContext`] for runtime validators.
#[must_use]
pub fn build_diagnostic_collector() -> Arc<DiagnosticCollector> {
    DiagnosticCollector::shared()
}

/// Build the seven-tier skill search-path list per
/// `norn-skills` DESIGN.md §D1, with any `settings.skills.search_paths`
/// entries prepended in source order.
///
/// Default ordering (first entry searched first):
///
/// 1. `settings.skills.search_paths[*]` (relative entries are joined
///    onto `cwd`).
/// 2. `{cwd}/.norn/skills/` — project Norn tier (highest priority of
///    the defaults).
/// 3. `{cwd}/.agents/skills/` — cross-client project tier.
/// 4. `{cwd}/.claude/skills/` — Claude Code project tier.
/// 5. `~/.norn/skills/` — user Norn tier.
/// 6. `~/.agents/skills/` — cross-client user tier.
/// 7. `~/.claude/skills/` — Claude Code user tier.
/// 8. `{cwd}/.meridian/skills/` — Meridian-integrated workspaces
///    (lowest priority).
///
/// Tiers whose home-dir resolution fails (CI/chroot environments
/// without a `HOME`) are silently omitted; the project tiers always
/// resolve because `cwd` is known.
#[must_use]
pub fn build_skill_search_paths(settings: &NornSettings, cwd: &Path) -> Vec<PathBuf> {
    use crate::config::paths::{
        project_agents_skills_dir, project_claude_skills_dir, project_meridian_skills_dir,
        project_skills_dir, user_agents_skills_dir, user_claude_skills_dir, user_skills_dir,
    };

    let mut paths: Vec<PathBuf> = Vec::new();

    if let Some(skills) = settings.skills.as_ref()
        && let Some(entries) = skills.search_paths.as_ref()
    {
        for entry in entries {
            paths.push(resolve_search_path(cwd, entry));
        }
    }

    paths.push(project_skills_dir(cwd));
    paths.push(project_agents_skills_dir(cwd));
    paths.push(project_claude_skills_dir(cwd));
    if let Some(dir) = user_skills_dir() {
        paths.push(dir);
    }
    if let Some(dir) = user_agents_skills_dir() {
        paths.push(dir);
    }
    if let Some(dir) = user_claude_skills_dir() {
        paths.push(dir);
    }
    paths.push(project_meridian_skills_dir(cwd));

    paths
}

/// Scan the supplied search paths and return a populated
/// [`SkillCatalog`] wrapped in `Arc`.
///
/// `SkillCatalog::scan` silently skips missing directories (logged at
/// `tracing::debug!` level) so passing the full seven-tier list when
/// only one tier exists is the intended pattern.
#[must_use]
pub fn build_skill_catalog(paths: &[PathBuf]) -> Arc<SkillCatalog> {
    Arc::new(SkillCatalog::scan(paths))
}

/// Resolve a settings-supplied path string against the working
/// directory. Absolute paths are returned verbatim; relative paths are
/// joined onto `cwd`.
fn resolve_search_path(cwd: &Path, raw: &str) -> PathBuf {
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        candidate
    } else {
        cwd.join(candidate)
    }
}

/// Resolve the [`LengthLimit`] applied to the `Write` tool from the
/// profile's `[tool_config.write]` section and an optional CLI override.
///
/// Resolution order:
///
/// 1. If the profile has no `tool_config.write` section, start from
///    [`LengthLimit::none`].
/// 2. Otherwise, deserialise it into [`WriteToolSpec`] — `max_code_lines`
///    becomes the default and `length_overrides` populate the
///    glob/limit pairs (in source order — first match wins per
///    [`LengthLimit::limit_for`]).
/// 3. If `cli_override` is `Some`, replace `default` with the CLI value.
///    Glob overrides from the profile are preserved.
///
/// # Errors
///
/// Returns [`BuildError::Argument`] when the profile section cannot be
/// deserialised (missing fields, wrong types) or when any
/// `length_overrides[i].pattern` fails [`glob::Pattern::new`].
pub fn length_limit_from_profile(
    profile: &Profile,
    cli_override: Option<usize>,
) -> Result<LengthLimit, BuildError> {
    let mut limit = match profile
        .settings
        .get("tool_config")
        .and_then(|tool_config| tool_config.get("write"))
    {
        Some(raw) => {
            let spec: WriteToolSpec = serde_json::from_value(raw.clone()).map_err(|err| {
                BuildError::Argument(format!(
                    "invalid [tool_config.write] profile section: {err} (expected optional \
                     max_code_lines: usize and optional length_overrides: \
                     [{{ pattern: string, limit: usize }}])",
                ))
            })?;
            spec.into_length_limit()?
        }
        None => LengthLimit::none(),
    };
    if let Some(value) = cli_override {
        limit.default = Some(value);
    }
    Ok(limit)
}

/// Construct a [`WriteTool`] whose [`LengthLimit`] is resolved from the
/// profile's `[tool_config.write]` section and the `-c
/// write.max_code_lines=N` override carried on [`ConfigOverrides`].
///
/// Returned tool is ready to register with [`norn::tool::registry::ToolRegistry`].
/// Profile glob overrides are preserved when the CLI override is set;
/// the CLI override only replaces `LengthLimit::default`.
///
/// # Errors
///
/// Propagates [`length_limit_from_profile`] errors verbatim.
pub fn build_write_tool(
    profile: &Profile,
    overrides: &ConfigOverrides,
) -> Result<WriteTool, BuildError> {
    let limit = length_limit_from_profile(profile, overrides.write_max_code_lines)?;
    Ok(WriteTool::with_length_limit(limit))
}

/// Parse the optional `[iteration_monitor]` section from
/// [`Profile::settings`] into a typed [`IterationMonitorConfig`].
///
/// Returns [`None`] when the profile does not contain an
/// `iteration_monitor` key. Returns [`BuildError::Argument`] when the
/// value is present but cannot be deserialised into the expected shape.
///
/// # Errors
///
/// Propagates serde failures as [`BuildError::Argument`] with the
/// underlying message so the caller surfaces them as exit code 2.
pub fn iteration_monitor_from_profile(
    profile: &Profile,
) -> Result<Option<IterationMonitorConfig>, BuildError> {
    let Some(raw) = profile.settings.get("iteration_monitor") else {
        return Ok(None);
    };
    let spec: IterationMonitorSpec = serde_json::from_value(raw.clone()).map_err(|err| {
        BuildError::Argument(format!(
            "invalid [iteration_monitor] profile section: {err} (expected fields: \
             context_window_tokens, warn_threshold_pct, handoff_threshold_pct, \
             handoff_guidance, failure_repeat_window, hedging_patterns)",
        ))
    })?;
    Ok(Some(spec.into_config()))
}

/// CLI-side mirror of [`IterationMonitorConfig`] used for deserialisation
/// only. Carries the same field set as the libnorn struct but adds the
/// `Deserialize` derive that libnorn cannot ship without taking serde as
/// a dependency.
#[derive(Debug, Deserialize)]
struct IterationMonitorSpec {
    context_window_tokens: u64,
    warn_threshold_pct: f64,
    handoff_threshold_pct: f64,
    handoff_guidance: String,
    failure_repeat_window: usize,
    #[serde(default)]
    hedging_patterns: Vec<String>,
}

impl IterationMonitorSpec {
    fn into_config(self) -> IterationMonitorConfig {
        IterationMonitorConfig {
            context_window_tokens: self.context_window_tokens,
            warn_threshold_pct: self.warn_threshold_pct,
            handoff_threshold_pct: self.handoff_threshold_pct,
            handoff_guidance: self.handoff_guidance,
            failure_repeat_window: self.failure_repeat_window,
            hedging_patterns: self.hedging_patterns,
        }
    }
}

/// Build a [`ToolContext`] pre-loaded with the diagnostics post-validation
/// check and `DiagnosticInfra` extension.
///
/// The returned context should be set on the [`ToolRegistry`] via
/// [`ToolRegistry::with_context`] or [`ToolRegistry::set_context`] BEFORE
/// other extensions are installed (they go through `insert_extension` on
/// the same `Arc`).
///
/// LD-015 R3: when the caller supplies an `lsp_workspace` and/or
/// `lsp_backend`, those slots are forwarded to
/// [`build_diagnostic_infra`] so the post-check pipeline gets a fast
/// LSP path before falling back to the adapter-subprocess cascade. The
/// TUI driver builds a single `Arc<LspWorkspace>` at startup and threads
/// it through `RuntimeInputs` so every agent step queries the same
/// language-server processes. `None` for either slot keeps the
/// corresponding feature dark — graceful CO5 degradation.
pub fn build_tool_context_with_diagnostics(
    workspace_root: &std::path::Path,
    working_dir: norn::tool::context::SharedWorkingDir,
    lsp_backend: Option<Arc<dyn norn::tools::lsp::LspBackend>>,
    lsp_workspace: Option<&norn::tools::lsp::LspWorkspace>,
) -> norn::tool::context::ToolContext {
    use norn::tools::diagnostics::{DiagnosticsPostCheck, build_diagnostic_infra};

    let infra = build_diagnostic_infra(workspace_root, lsp_backend, lsp_workspace);
    let mut ctx = norn::tool::context::ToolContext::with_working_dir(working_dir);
    ctx.insert_extension(Arc::new(infra));
    ctx.post_checks.push(Box::new(DiagnosticsPostCheck));
    ctx
}

/// Install an [`ActionLog`](norn::session::action_log::ActionLog) on the
/// registry's shared [`norn::tool::context::ToolContext`] and the
/// [`norn::agent_loop::loop_context::LoopContext`] so the `action_log` tool
/// and the loop's dispatch recording share one ledger.
///
/// The log is constructed with the loop context's **live**
/// [`norn::tool::context::SharedWorkingDir`] handle (the same one bash's
/// `cd` parsing updates), so model-supplied relative paths resolve
/// against the agent's working directory rather than the process CWD.
///
/// Must be called after `open_session` (the event store does not exist at
/// `build_runtime` time). On `--resume` / `--fork`, `store` already
/// contains the replayed events: [`norn::agent::rebuild_action_log`]
/// replays them so the resumed session's action ledger (and derived
/// mutation ledger) is queryable instead of starting empty. A fresh
/// store has no events, making the rebuild a no-op.
pub fn install_action_log(
    registry: &norn::tool::registry::ToolRegistry,
    store: &Arc<EventStore>,
    loop_context: &mut norn::agent_loop::loop_context::LoopContext,
) {
    let action_log = Arc::new(norn::session::action_log::ActionLog::with_working_dir(
        Arc::clone(store),
        loop_context.working_dir.clone(),
    ));
    norn::agent::rebuild_action_log(&action_log, &store.events());
    if let Some(ctx) = registry.shared_context() {
        ctx.insert_extension(Arc::clone(&action_log));
    }
    loop_context.action_log = Some(action_log);
}

/// Build a [`SlashState`] and [`SlashCommandRegistry`] from a completed runtime bundle.
pub fn build_slash_state_from_bundle(
    cli: &Cli,
    bundle: &RuntimeBundle,
    store: Arc<EventStore>,
    session_id: Option<String>,
) -> (SlashState, SlashCommandRegistry) {
    build_slash_state_inner(cli, bundle, store, session_id, None)
}

/// Variant that accepts a pre-parsed output schema, avoiding a
/// redundant re-parse when the caller has already validated the
/// `--output-schema` flag (e.g. the print-mode orchestrator).
pub fn build_slash_state_with_schema(
    cli: &Cli,
    bundle: &RuntimeBundle,
    store: Arc<EventStore>,
    session_id: Option<String>,
    output_schema: Option<Value>,
) -> (SlashState, SlashCommandRegistry) {
    build_slash_state_inner(cli, bundle, store, session_id, output_schema)
}

fn build_slash_state_inner(
    cli: &Cli,
    bundle: &RuntimeBundle,
    store: Arc<EventStore>,
    session_id: Option<String>,
    output_schema_override: Option<Value>,
) -> (SlashState, SlashCommandRegistry) {
    let tools: Vec<(String, String)> = bundle
        .registry
        .names()
        .filter_map(|name| {
            bundle
                .registry
                .get(name)
                .map(|tool| (tool.name().to_owned(), tool.description().to_owned()))
        })
        .collect();

    let variable_pairs = parse_variable_pairs(&cli.variables);
    let output_schema = output_schema_override
        .or_else(|| parse_output_schema_for_state(cli.output_schema.as_deref()));

    let seed = SlashStateSeed {
        model: bundle.model.clone(),
        output_schema,
        session_name: cli.session_name.clone(),
        session_id,
        data_dir: session_data_dir(),
        no_session: cli.no_session,
        variable_pairs,
        tools,
        store,
    };
    let state = SlashState::new(seed);
    let registry = build_slash_registry(&state, None);
    (state, registry)
}
fn parse_variable_pairs(raw: &[String]) -> Vec<(String, String)> {
    raw.iter()
        .filter_map(|pair| match parse_kv(pair) {
            Ok(kv) => Some(kv),
            Err(err) => {
                tracing::warn!(
                    pair = %pair,
                    error = %err,
                    "skipping malformed --variables pair when building slash state",
                );
                None
            }
        })
        .collect()
}

fn parse_output_schema_for_state(raw: Option<&str>) -> Option<serde_json::Value> {
    let value = raw?;
    match parse_inline_or_file(value) {
        Ok(parsed) => Some(parsed),
        Err(err) => {
            tracing::warn!(
                raw = %value,
                error = %err,
                "skipping unparseable --output-schema when building slash state",
            );
            None
        }
    }
}

/// CLI-side mirror of the `[tool_config.write]` profile section.
///
/// Both fields are optional — a profile may set just `max_code_lines`,
/// just `length_overrides`, both, or neither — and any combination
/// resolves to a valid [`LengthLimit`].
#[derive(Debug, Default, Deserialize)]
struct WriteToolSpec {
    #[serde(default)]
    max_code_lines: Option<usize>,
    #[serde(default)]
    length_overrides: Vec<LengthOverrideSpec>,
}

/// One entry in `[tool_config.write.length_overrides]`. The `pattern`
/// must compile via [`glob::Pattern::new`]; failures surface as
/// [`BuildError::Argument`] naming the offending pattern.
#[derive(Debug, Deserialize)]
struct LengthOverrideSpec {
    pattern: String,
    limit: usize,
}

impl WriteToolSpec {
    fn into_length_limit(self) -> Result<LengthLimit, BuildError> {
        let mut limit = LengthLimit {
            default: self.max_code_lines,
            overrides: Vec::with_capacity(self.length_overrides.len()),
        };
        for entry in self.length_overrides {
            let pattern = glob::Pattern::new(&entry.pattern).map_err(|err| {
                BuildError::Argument(format!(
                    "invalid glob pattern '{}' in [tool_config.write.length_overrides]: {err}",
                    entry.pattern,
                ))
            })?;
            limit.overrides.push((pattern, entry.limit));
        }
        Ok(limit)
    }
}
