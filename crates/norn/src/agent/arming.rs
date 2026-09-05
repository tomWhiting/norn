//! Shared launch-path arming mechanisms for
//! [`AgentBuilder::build`](crate::agent::builder::AgentBuilder::build) and
//! the spawn/fork/rhai child launch paths.
//!
//! Split out of `agent/assembly.rs` to keep it within the production-size
//! limit. These are the single shared mechanisms every agent launch path
//! (root, spawned child, rhai-spawned child, fork) uses so the auto-compaction
//! trigger, the in-session schedule executor, and the "# Available Skills"
//! prompt listing cannot drift between root and children.

use std::sync::Arc;

use parking_lot::RwLock;
use uuid::Uuid;

use crate::agent::PendingMailboxLease;
use crate::agent::pending_messages::PendingAgentMessages;
use crate::agent::process_delivery::ProcessMessageDelivery;
use crate::agent::registry::AgentRegistry;
use crate::error::ConfigError;
use crate::r#loop::config::AgentLoopConfig;
use crate::r#loop::inbound::InboundSender;
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::tokens::SimpleTokenEstimator;
use crate::model_selection::CatalogBackend;
use crate::process::{ProcessManager, ProcessManagerGuard};
use crate::session::MailboxId;
use crate::session::context_edit::ContextEdits;
use crate::session::store::EventStore;
use crate::tool::context::{SessionId, ToolContext};
use crate::tool::registry::ToolRegistry;
use crate::tools::agent::{AgentModel, AgentWakeRegistry};

pub(crate) use super::skill_prompt::{
    apply_skill_listing, child_skill_tool_available, install_child_skill_listing,
};

/// Publish tool definitions plus the model and effort inherited by children.
///
/// The caller publishes the source-aware parent prompt plan after this shared
/// arming step. [`crate::agent::fork::ParentSystemInstruction`] remains an
/// input-only compatibility bridge for legacy embedders and is never emitted
/// by assembled Norn contexts.
pub(crate) fn publish_parent_execution_context(
    registry: &ToolRegistry,
    context: &ToolContext,
    loop_context: &LoopContext,
    model: &str,
) {
    crate::agent::assembly::install_tool_catalog(registry, context);
    context.insert_extension(Arc::new(AgentModel {
        model: model.to_owned(),
        reasoning_effort: loop_context.reasoning_effort,
    }));
}

/// Model-bound window provenance published by the current agent's owner.
/// An absent explicit value stays absent even after catalogue arming.
pub(crate) struct ContextWindowPolicy {
    backend: Option<CatalogBackend>,
    model: String,
    explicit_window: Option<u64>,
}

impl ContextWindowPolicy {
    /// Publish this agent's policy for its own descendants, without putting a
    /// model-bound value into the model-independent delegation grant.
    pub(crate) fn publish(self, context: &ToolContext) {
        context.insert_extension(Arc::new(self));
    }
}

/// A validated child window paired with its explicit/derived provenance.
/// The effective value arms the child's tools as well as its agent loop.
pub(crate) struct ResolvedChildWindow {
    policy: ContextWindowPolicy,
    window: u64,
}

impl ResolvedChildWindow {
    /// Install the child's own effective tool budget and window provenance.
    /// Child contexts are fresh; forwarding the parent's budget would be
    /// incorrect when the child selects another model or explicit window.
    pub(crate) fn publish(self, context: &ToolContext) {
        crate::runtime_init::install_tool_output_budget(context, Some(self.window));
        self.policy.publish(context);
    }
}

/// Retain the root's operator-explicit window at build and live selection.
pub(crate) fn publish_parent_context_window(
    context: &ToolContext,
    selection: &crate::model_selection::ModelRuntime,
) {
    ContextWindowPolicy {
        backend: selection.backend(),
        model: selection.model().to_owned(),
        explicit_window: selection.explicit_window(),
    }
    .publish(context);
}

/// Resolve a child override, then an explicit window belonging to the same
/// live parent model and concrete route, then the child's own catalogue.
/// A stale policy, a different route/model, or a derived window never supplies
/// an implicit override. The returned policy preserves that distinction for
/// descendants and is published only after the child's context is assembled.
///
/// # Errors
/// Refuses missing, zero, or over-ceiling context windows before child admission.
pub(crate) fn resolve_child_context_window(
    parent: Option<&ToolContext>,
    backend: Option<CatalogBackend>,
    config: &mut AgentLoopConfig,
    model: &str,
) -> Result<ResolvedChildWindow, ConfigError> {
    let inherited = parent.and_then(|context| {
        let policy = context.get_extension::<ContextWindowPolicy>()?;
        let live = context.get_extension::<AgentModel>()?;
        (backend.is_some()
            && policy.backend == backend
            && policy.model == model
            && live.model == model)
            .then_some(policy.explicit_window)
            .flatten()
    });
    let explicit_window = config.context_window_limit.or(inherited);
    config.context_window_limit = explicit_window;
    let window = arm_child_window(backend, config, model)?;
    Ok(ResolvedChildWindow {
        policy: ContextWindowPolicy {
            backend,
            model: model.to_owned(),
            explicit_window,
        },
        window,
    })
}

/// Arm auto-compaction on a loop context and its effective agent-loop
/// config — the single shared mechanism every agent launch path (root,
/// spawned child, rhai-spawned child, fork) uses, so the trigger cannot
/// drift between them.
///
/// Installs the token estimator and the [`ContextEdits`] tracker on the
/// loop context (the preflight needs both: the estimator to size each
/// request, the tracker for the usage floor and the compaction commit),
/// and fills an unset `context_window_limit` from the model catalog for
/// *this agent's* resolved model. An explicit window — from settings, a
/// `-c` override, or a child policy — always wins because the fill runs
/// only when the merged value is still `None`. Missing metadata without
/// an applicable explicit window stays unresolved here and is refused
/// by the launch-path window guard. The reserve default
/// (`AgentLoopConfig::default().auto_compact_reserve_tokens`) already
/// flows through the config and is not touched here.
pub(crate) fn arm_auto_compaction(
    backend: Option<CatalogBackend>,
    loop_context: &mut LoopContext,
    config: &mut AgentLoopConfig,
    model: &str,
) {
    loop_context.token_estimator = Some(Arc::new(SimpleTokenEstimator));
    loop_context.context_edits = Some(ContextEdits::new());
    if config.context_window_limit.is_none() {
        config.context_window_limit = backend
            .and_then(|route| route.model(model))
            .map(|entry| entry.context_window);
    }
}

/// Validate the armed context window against the model catalog — the
/// post-arming guard for the 2026-07-05 incident (owner-ruled, Tom):
/// the window is set by the model unless an override is wanted, an
/// override above the selected route's declared ceiling is an error, and
/// missing route metadata requires an explicit window.
///
/// Two rejections, both loud, never a silent clamp (a clamp hides config
/// drift — the incident's global 272k override on a 128k model would
/// have become an invisible mystery):
///
/// - **Explicit window above the model's ceiling.** For a catalogued
///   model, an armed window above
///   [`ModelEntry::max_context_window`](crate::model_catalog::ModelEntry::max_context_window)
///   can only come from explicit config (the fill never exceeds the
///   catalog) and means every protection threshold sits beyond the real
///   wall — token warnings and auto-compaction mathematically cannot
///   fire before the provider rejects.
/// - **No window at all.** After the fill, a `None` window means the
///   model is not in the catalog AND no explicit window was supplied;
///   running would silently disable the protections, which is the ruled-
///   against state.
///
/// Called by `AgentBuilder::build` for the root (covering TUI, print,
/// and driven mode through the one shared assembly funnel) and — via
/// [`arm_child_window`] — by every child launch path (spawn, fork, rhai),
/// so no agent at any depth ever launches with a lying window.
pub(crate) fn validate_context_window(
    backend: Option<CatalogBackend>,
    config: &AgentLoopConfig,
    model: &str,
) -> Result<(), ConfigError> {
    crate::model_selection::resolve_window(backend, model, config.context_window_limit)?;
    config
        .context_window_limit
        .ok_or_else(|| ConfigError::InvalidConfig {
            reason: format!(
                "context window for model '{model}' was not resolved before agent arming"
            ),
        })?;
    Ok(())
}

/// Resolve and validate a child's context window (owner ruling
/// 2026-07-07: the child's window comes from the model catalog,
/// **overrideable** per child) — the child-path counterpart of the root
/// builder's arm-then-validate sequence, called by every child launch
/// site (spawn, fork, rhai) BEFORE the launch commits (before the
/// registry reservation is confirmed), so a failure aborts the launch as
/// a typed error instead of running a child whose token warnings and
/// auto-compaction can never fire.
///
/// Resolution order:
///
/// 1. **Explicit override** — `child_policy.loop_config.context_window`,
///    already resolved into `config.context_window_limit` by
///    [`ChildLoopConfig::to_loop_config`](crate::agent::child_policy::ChildLoopConfig::to_loop_config)
///    — wins, with the root's explicit-window semantics
///    ([`validate_context_window`]'s ceiling branch): a value above a
///    catalogued model's maximum is rejected (never a silent clamp), and
///    a deliberate uncatalogued model is accepted with the override
///    armed. The rejection names the child override; the root's explicit
///    configuration applies only through the same-model, same-route
///    inheritance performed by [`resolve_child_context_window`].
/// 2. Else **catalog fill** for the child's own resolved model —
///    mirroring [`arm_auto_compaction`]'s fill exactly (and idempotent
///    with it: the later arming call finds the window set and leaves it
///    untouched).
/// 3. Else a typed error naming the child remedies: a catalogued model,
///    or the explicit `child_policy.loop_config.context_window`
///    override. [`resolve_child_context_window`] additionally supplies a
///    parent's operator-explicit window before this guard, but only for the
///    same live model and concrete backend.
///
/// # Errors
///
/// [`ConfigError::InvalidConfig`], worded with child remedies only, per
/// the two rejection cases above.
pub(crate) fn arm_child_window(
    backend: Option<CatalogBackend>,
    config: &mut AgentLoopConfig,
    model: &str,
) -> Result<u64, ConfigError> {
    if config.context_window_limit == Some(0) {
        return Err(ConfigError::InvalidConfig {
            reason: format!(
                "child_policy.loop_config.context_window must be greater than zero for model '{model}'"
            ),
        });
    }
    if let Some(limit) = config.context_window_limit {
        // The explicit child override: the same ceiling rule as the
        // root's explicit window (the fill below never exceeds the
        // catalog, so an over-ceiling value can only be the override).
        if let Some(max) = backend
            .and_then(|route| route.model(model))
            .map(|entry| entry.max_context_window)
            && limit > max
        {
            return Err(ConfigError::InvalidConfig {
                reason: format!(
                    "child context window {limit} exceeds model '{model}'s maximum of \
                     {max} (model catalog) — token warnings and auto-compaction would \
                     sit beyond the real window and never fire. Lower or remove \
                     child_policy.loop_config.context_window; with no override the \
                     child's window is taken from the model catalog",
                ),
            });
        }
        return Ok(limit);
    }
    config.context_window_limit = backend
        .and_then(|route| route.model(model))
        .map(|entry| entry.context_window);
    config
        .context_window_limit
        .ok_or_else(|| ConfigError::InvalidConfig {
            reason: format!(
                "{}: the selected route declares no capability metadata for child \
                 model '{model}', and no applicable explicit context window was supplied. \
                 Set child_policy.loop_config.context_window explicitly, or use the \
                 same model and route as a parent with an operator-explicit window; \
                 without a window, token warnings and auto-compaction cannot fire",
                backend.map_or_else(
                    || "provider without a model catalogue".to_owned(),
                    |route| format!("{}.{}", route.provider, route.backend),
                ),
            ),
        })
}

/// Where a child's resolved reasoning effort came from — decides how
/// [`arm_child_reasoning_effort`] handles a catalog-unsupported pairing.
pub(crate) enum ChildEffortSource<'a> {
    /// Explicitly configured for this child; the label names the exact
    /// setting (e.g. `variants.scout.reasoning_effort`, a profile's
    /// `reasoning_effort`) so the rejection is actionable.
    Explicit(&'a str),
    /// Ambient inheritance from the parent's live effort (owner ruling
    /// 2026-07-07); `child` labels the child for the degrade warning.
    Inherited {
        /// The child's role/variant label (or `"fork"`).
        child: &'a str,
    },
}

/// Validate a child's resolved reasoning effort against the model catalog
/// for the CHILD's resolved model — the child-path counterpart of the
/// root's `--reasoning-effort` / `/effort` enforcement
/// ([`supports_effort`](crate::model_selection::supports_effort)),
/// called by every child launch site (spawn, fork, rhai) so an
/// unsupported pairing surfaces at launch instead of as an opaque
/// provider rejection (or a lenient backend's silent drop) after the
/// reservation and audit persist.
///
/// Root parity is exact, including uncatalogued models: the root REFUSES
/// an explicit effort on a model the catalog cannot vouch for (the
/// support check is catalog-membership-based), so a child does too.
///
/// - **Explicitly configured** effort unsupported by the child's model →
///   typed error naming the setting and the model's catalogued efforts.
/// - **Inherited** effort unsupported → degrade to `None` with a
///   `tracing::warn!` naming the child, model, and dropped effort: the
///   caller configured nothing wrong on this spawn, so failing it would
///   punish ambient inheritance — but the drop is never silent.
///
/// # Errors
///
/// [`ConfigError::InvalidConfig`] for the explicit-unsupported case only.
pub(crate) fn arm_child_reasoning_effort(
    backend: Option<CatalogBackend>,
    effort: Option<crate::provider::request::ReasoningEffort>,
    source: &ChildEffortSource<'_>,
    model: &str,
) -> Result<Option<crate::provider::request::ReasoningEffort>, ConfigError> {
    let Some(value) = effort else {
        return Ok(None);
    };
    if crate::model_selection::supports_effort(backend, model, value) {
        return Ok(Some(value));
    }
    let label = crate::r#loop::commands::effort_label(value);
    match source {
        ChildEffortSource::Explicit(setting) => Err(ConfigError::InvalidConfig {
            reason: format!(
                "{setting}: {}",
                crate::model_selection::effort_refusal_message(backend, model, value),
            ),
        }),
        ChildEffortSource::Inherited { child } => {
            tracing::warn!(
                child = %child,
                model = %model,
                effort = %label,
                backend = ?backend,
                "inherited reasoning effort is not declared for this route/model; \
                 running the child with no reasoning effort",
            );
            Ok(None)
        }
    }
}

/// Inputs that bind the root schedule executor to its session and mailbox.
pub(crate) struct RootScheduleExecutorParts<'a> {
    /// Shared tool context where the schedule handle is installed.
    pub(crate) shared: &'a ToolContext,
    /// Durable event store used to rebuild schedules and pending messages.
    pub(crate) event_store: &'a Arc<EventStore>,
    /// Runtime identifier for the root agent.
    pub(crate) agent_id: Uuid,
    /// Stable mailbox identity for this session generation.
    pub(crate) mailbox_id: MailboxId,
    /// Controller-liveness proof retained by the root loop context.
    pub(crate) mailbox_lease: &'a Arc<PendingMailboxLease>,
    /// Live inbound route, when the root has one.
    pub(crate) inbound_tx: Option<InboundSender>,
    /// Agent registry consulted by schedule delivery, when coordination exists.
    pub(crate) agent_registry: Option<Arc<RwLock<AgentRegistry>>>,
}

/// Arm the root agent's in-session schedule executor (N-026) — the root
/// half of the shared mechanism the spawn/fork launch paths mirror at
/// their own construction sites, exactly like [`arm_auto_compaction`].
///
/// Rebuilds the [`ScheduleStore`](crate::schedule::ScheduleStore) from the
/// session's `schedule.*` events (a fresh session arms empty; a resume
/// re-arms survivors — past-due one-shots fire immediately marked late,
/// recurring schedules re-arm from resume time with no backfill), installs
/// the [`ScheduleHandle`](crate::schedule::ScheduleHandle) extension the
/// `cron` tool resolves, spawns the live executor, and binds its guard to
/// the loop context so dropping the agent aborts the timer task — timers
/// die with the process; only the event record survives, for resume.
///
/// When no agent coordination is installed the root still gets a durable
/// pending store (rebuilt from events, exactly as `install_agent_infra`
/// builds one) so a fired schedule with no live channel is queued somewhere
/// the next step's pending flush actually reads.
///
/// An embedder that hand-rolls
/// [`run_agent_step`](crate::agent_loop::runner::run_agent_step) without
/// going through assembly never calls this and therefore has no executor
/// and no `cron` tool — the same discoverable contract as
/// [`arm_auto_compaction`]'s.
pub(crate) fn arm_root_schedule_executor(
    loop_context: &mut LoopContext,
    parts: RootScheduleExecutorParts<'_>,
) -> Result<(), crate::error::SessionError> {
    if loop_context.pending_agent_messages.is_none() {
        loop_context.pending_agent_messages = Some(Arc::new(PendingAgentMessages::from_events(
            parts.agent_id,
            parts.mailbox_id,
            &parts.event_store.events(),
        )?));
    }
    if let Some(pending) = loop_context.pending_agent_messages.as_ref() {
        pending.register_root_mailbox(
            parts.agent_id,
            parts.mailbox_id,
            parts.event_store,
            parts.mailbox_lease,
        )?;
    }
    let schedule_store = Arc::new(crate::schedule::ScheduleStore::from_events(
        &parts.event_store.events(),
        chrono::Utc::now(),
    ));
    loop_context.schedule_executor = Some(crate::schedule::arm_schedule_executor(
        parts.shared,
        schedule_store,
        crate::schedule::ScheduleDelivery {
            agent_id: parts.agent_id,
            inbound: parts.inbound_tx,
            pending: loop_context.pending_agent_messages.clone(),
            event_store: Arc::clone(parts.event_store),
            registry: parts.agent_registry,
            wake_registry: parts
                .shared
                .get_extension::<crate::tools::agent::AgentWakeRegistry>(),
        },
    ));
    Ok(())
}

/// Arm an agent's background-process manager (NP-001) — the single shared
/// mechanism every launch path (root build, spawn, fork) uses, so the manager
/// wiring cannot drift between root and children, exactly like
/// [`arm_root_schedule_executor`] and its child counterparts.
///
/// Builds the durable completion/watch-alert sink ([`ProcessMessageDelivery`])
/// from the same handles the schedule executor uses, constructs a [`ProcessManager`]
/// whose spools live under this agent's session (or a per-run UUID when no
/// [`SessionId`] is installed), installs it as a `ToolContext` extension (the
/// `process` tool resolves it), and binds its [`ProcessManagerGuard`] to the
/// loop context so dropping the agent kills every still-running process group.
/// Processes are in-session state: a resumed session starts with an empty
/// registry (spools remain on disk), so nothing is rebuilt from events here.
///
/// Call after scheduling is armed, which ensures the durable pending store
/// exists — the completion sink queues into the same store.
pub(crate) fn arm_process_manager(
    shared: &ToolContext,
    loop_context: &mut LoopContext,
    event_store: &Arc<EventStore>,
    agent_id: Uuid,
    inbound_tx: Option<InboundSender>,
    agent_registry: Option<Arc<RwLock<AgentRegistry>>>,
) {
    let session_id = shared.get_extension::<SessionId>().map(|s| s.0.clone());
    let sink = Arc::new(ProcessMessageDelivery {
        agent_id,
        inbound: inbound_tx,
        pending: loop_context.pending_agent_messages.clone(),
        event_store: Arc::clone(event_store),
        registry: agent_registry,
        wake_registry: shared.get_extension::<AgentWakeRegistry>(),
    });
    let manager = Arc::new(ProcessManager::new(session_id, Some(sink)));
    shared.insert_extension(Arc::clone(&manager));
    loop_context.process_manager = Some(ProcessManagerGuard::new(manager));
}

#[cfg(test)]
mod tests;
