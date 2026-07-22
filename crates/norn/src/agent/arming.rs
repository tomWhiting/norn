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
/// `-c` override, or any future child-policy field — always wins because
/// the fill runs only when the merged value is still `None`. A model
/// absent from the catalog keeps `None`, which leaves the trigger
/// disabled (`maybe_auto_compact` returns early on a `None` window),
/// matching the root behavior exactly. The reserve default
/// (`AgentLoopConfig::default().auto_compact_reserve_tokens`) already
/// flows through the config and is not touched here.
pub(crate) fn arm_auto_compaction(
    loop_context: &mut LoopContext,
    config: &mut AgentLoopConfig,
    model: &str,
) {
    loop_context.token_estimator = Some(Arc::new(SimpleTokenEstimator));
    loop_context.context_edits = Some(ContextEdits::new());
    if config.context_window_limit.is_none() {
        config.context_window_limit =
            crate::model_catalog::smallest_context_window_for_model(model);
    }
}

/// Validate the armed context window against the model catalog — the
/// post-arming guard for the 2026-07-05 incident (owner-ruled, Tom):
/// the window is set by the model unless an override is wanted, an
/// override the model cannot honour is an error, and an unknown model is
/// an error ("it probably means the wrong model code").
///
/// Two rejections, both loud, never a silent clamp (a clamp hides config
/// drift — the incident's global 272k override on a 128k model would
/// have become an invisible mystery):
///
/// - **Explicit window above the model's ceiling.** For a catalogued
///   model, an armed window above
///   [`largest_max_context_window_for_model`](crate::model_catalog::largest_max_context_window_for_model)
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
    config: &AgentLoopConfig,
    model: &str,
) -> Result<(), ConfigError> {
    match config.context_window_limit {
        Some(limit) => {
            if let Some(max) = crate::model_catalog::largest_max_context_window_for_model(model)
                && limit > max
            {
                return Err(ConfigError::InvalidConfig {
                    reason: format!(
                        "configured context window {limit} exceeds model '{model}'s maximum \
                         of {max} (model catalog) — token warnings and auto-compaction would \
                         sit beyond the real window and never fire. Remove or lower the \
                         explicit window (settings agent.context_window, -c context_window, \
                         or the builder limit); with no explicit value the window is taken \
                         from the model catalog",
                    ),
                });
            }
            Ok(())
        }
        None => Err(ConfigError::InvalidConfig {
            reason: format!(
                "model '{model}' is not in the model catalog and no context window is \
                 configured — check the model id for a typo first. For a deliberate \
                 uncatalogued model, set agent.context_window in settings, pass \
                 -c context_window=<tokens>, or set the builder's context window; without \
                 one, token warnings and auto-compaction stay disabled",
            ),
        }),
    }
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
///    armed. The rejection is worded with the CHILD remedy, not the
///    root-only knobs (settings `agent.context_window`, `-c` overrides,
///    the builder window), which do not exist on the child path.
/// 2. Else **catalog fill** for the child's own resolved model —
///    mirroring [`arm_auto_compaction`]'s fill exactly (and idempotent
///    with it: the later arming call finds the window set and leaves it
///    untouched).
/// 3. Else a typed error naming the child remedies: a catalogued model,
///    or the explicit `child_policy.loop_config.context_window`
///    override.
///
/// # Errors
///
/// [`ConfigError::InvalidConfig`], worded with child remedies only, per
/// the two rejection cases above.
pub(crate) fn arm_child_window(
    config: &mut AgentLoopConfig,
    model: &str,
) -> Result<(), ConfigError> {
    if let Some(limit) = config.context_window_limit {
        // The explicit child override: the same ceiling rule as the
        // root's explicit window (the fill below never exceeds the
        // catalog, so an over-ceiling value can only be the override).
        if let Some(max) = crate::model_catalog::largest_max_context_window_for_model(model)
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
        return Ok(());
    }
    config.context_window_limit = crate::model_catalog::smallest_context_window_for_model(model);
    if config.context_window_limit.is_none() {
        return Err(ConfigError::InvalidConfig {
            reason: format!(
                "child model '{model}' is not in the model catalog — check the model \
                 id for a typo first. For a deliberate uncatalogued child model, use \
                 a catalogued model instead, or set \
                 child_policy.loop_config.context_window explicitly (owner ruling \
                 2026-07-07: the child's window comes from the model catalog, \
                 overrideable per child); without a window, token warnings and \
                 auto-compaction cannot fire",
            ),
        });
    }
    Ok(())
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
/// ([`reasoning_effort_supported_for_model`](crate::r#loop::commands::reasoning_effort_supported_for_model)),
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
    effort: Option<crate::provider::request::ReasoningEffort>,
    source: &ChildEffortSource<'_>,
    model: &str,
) -> Result<Option<crate::provider::request::ReasoningEffort>, ConfigError> {
    let Some(value) = effort else {
        return Ok(None);
    };
    if crate::r#loop::commands::reasoning_effort_supported_for_model(model, value) {
        return Ok(Some(value));
    }
    let label = crate::r#loop::commands::effort_label(value);
    match source {
        ChildEffortSource::Explicit(setting) => {
            let catalog_detail = crate::model_catalog::find_model(
                crate::model_catalog::DEFAULT_PROVIDER,
                crate::model_catalog::DEFAULT_BACKEND,
                model,
            )
            .map_or_else(
                || {
                    format!(
                        "model '{model}' is not in the model catalog, which therefore \
                         declares no supported efforts for it"
                    )
                },
                |entry| {
                    format!(
                        "model '{model}' supports: {} (model catalog)",
                        entry.supported_reasoning_efforts.join(", "),
                    )
                },
            );
            Err(ConfigError::InvalidConfig {
                reason: format!(
                    "reasoning effort '{label}' ({setting}) is not supported for the \
                     child's resolved model — {catalog_detail}. Set a supported effort \
                     there, or resolve the child onto a model that supports it",
                ),
            })
        }
        ChildEffortSource::Inherited { child } => {
            tracing::warn!(
                child = %child,
                model = %model,
                effort = %label,
                "inherited reasoning effort is not supported by the child's resolved \
                 model (model catalog); running the child with no reasoning effort — \
                 set a supported effort explicitly to silence this",
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
