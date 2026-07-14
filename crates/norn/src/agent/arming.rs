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

use crate::agent::fork::ParentSystemInstruction;
use crate::agent::pending_messages::PendingAgentMessages;
use crate::agent::process_delivery::ProcessMessageDelivery;
use crate::agent::registry::AgentRegistry;
use crate::error::ConfigError;
use crate::r#loop::config::AgentLoopConfig;
use crate::r#loop::inbound::InboundSender;
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::tokens::SimpleTokenEstimator;
use crate::process::{ProcessManager, ProcessManagerGuard};
use crate::session::context_edit::ContextEdits;
use crate::session::store::EventStore;
use crate::skill::SkillCatalog;
use crate::tool::context::{SessionId, ToolContext};
use crate::tool::registry::ToolRegistry;
use crate::tools::agent::{AgentModel, AgentWakeRegistry};

/// Publish the exact parent instruction, model, and effort inherited by child launches.
pub(crate) fn publish_parent_execution_context(
    registry: &ToolRegistry,
    context: &ToolContext,
    loop_context: &LoopContext,
    model: &str,
) {
    crate::agent::assembly::install_tool_catalog(registry, context);
    context.insert_extension(Arc::new(ParentSystemInstruction::new(
        loop_context.base_system_instruction(),
    )));
    context.insert_extension(Arc::new(AgentModel {
        model: model.to_owned(),
        reasoning_effort: loop_context.reasoning_effort,
    }));
}

/// Whether the `skill` tool is on a child's resolved tool surface: it must
/// be present (and un-gated) in the shared parent registry *and* admitted
/// by the child's allow-list — the same two filters
/// [`collect_function_definitions`](crate::provider::surface::collect_function_definitions)
/// applies to the child's tool definitions. A child's system prompt
/// advertises the skill listing only when this holds, so it never lists a
/// skill the child has no tool to load.
pub(crate) fn child_skill_tool_available(
    parent_registry: &ToolRegistry,
    allow_list: Option<&[String]>,
) -> bool {
    parent_registry.get("skill").is_some()
        && allow_list.is_none_or(|list| list.iter().any(|name| name == "skill"))
}

/// Apply the skill-catalog "# Available Skills" listing to a loop context's
/// `base_suffix` — the single shared mechanism the root builder and the
/// child launch paths use for the listing's content and gating, so the
/// section cannot drift between them.
///
/// Sets nothing when `skill_tool_available` is `false`: advertising a skill
/// the agent has no tool to load would be a lie. The content is the
/// catalog's filtered
/// [`SkillCatalog::system_prompt_listing`], identical for root and
/// children (an all-hidden or empty catalog yields an empty string, which
/// the system-prompt build omits).
pub(crate) fn apply_skill_listing(
    loop_context: &mut LoopContext,
    catalog: &SkillCatalog,
    skill_tool_available: bool,
) {
    if skill_tool_available {
        loop_context.base_suffix = catalog.system_prompt_listing();
    }
}

/// Give a spawned/forked child the same "# Available Skills" listing the
/// root gets.
///
/// Children build a bare [`LoopContext`] and never run the root's
/// `install_system_prompt` — the step that materializes `base_suffix` into
/// the system instruction — so this applies the shared listing via
/// [`apply_skill_listing`], then folds the child's base instruction into
/// `base_prefix` and rebuilds the base section, producing the same
/// base-instruction-then-listing layering the root emits. A no-op when the
/// resulting listing is empty (the skill tool is gated off for the child,
/// or the catalog is empty / all-hidden), leaving the child's system
/// instruction untouched.
pub(crate) fn install_child_skill_listing(
    loop_context: &mut LoopContext,
    catalog: &SkillCatalog,
    skill_tool_available: bool,
) {
    // An embedder-supplied parent base (`ParentSystemInstruction`) may
    // legitimately already contain the listing — the root's
    // `base_system_instruction()` includes its materialized `base_suffix`.
    // Appending again would duplicate the section, so the exact generated
    // listing text already present anywhere in the child's base is treated
    // as installed.
    let listing = catalog.system_prompt_listing();
    if !listing.is_empty()
        && loop_context
            .system_sections
            .first()
            .is_some_and(|base| base.contains(&listing))
    {
        return;
    }
    apply_skill_listing(loop_context, catalog, skill_tool_available);
    if loop_context.base_suffix.is_empty() {
        return;
    }
    if loop_context.base_prefix.is_empty() {
        loop_context.base_prefix = loop_context
            .system_sections
            .first()
            .cloned()
            .unwrap_or_default();
    }
    loop_context.rebuild_base_section();
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
    shared: &ToolContext,
    loop_context: &mut LoopContext,
    event_store: &Arc<EventStore>,
    agent_id: Uuid,
    inbound_tx: Option<crate::r#loop::inbound::InboundSender>,
    agent_registry: Option<Arc<RwLock<AgentRegistry>>>,
) {
    if loop_context.pending_agent_messages.is_none() {
        loop_context.pending_agent_messages = Some(Arc::new(PendingAgentMessages::from_events(
            &event_store.events(),
        )));
    }
    let schedule_store = Arc::new(crate::schedule::ScheduleStore::from_events(
        &event_store.events(),
        chrono::Utc::now(),
    ));
    loop_context.schedule_executor = Some(crate::schedule::arm_schedule_executor(
        shared,
        schedule_store,
        crate::schedule::ScheduleDelivery {
            agent_id,
            inbound: inbound_tx,
            pending: loop_context.pending_agent_messages.clone(),
            event_store: Arc::clone(event_store),
            registry: agent_registry,
            wake_registry: shared.get_extension::<crate::tools::agent::AgentWakeRegistry>(),
        },
    ));
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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
mod tests {
    use super::*;

    /// A `NORN_HOME` guard so a spawned process's spool lands under a temp dir,
    /// serialised via `#[serial]`.
    struct HomeGuard {
        prior: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn set(path: &std::path::Path) -> Self {
            let prior = std::env::var_os("NORN_HOME");
            // SAFETY: paired with `#[serial]`; no concurrent reader observes it.
            unsafe { std::env::set_var("NORN_HOME", path) };
            Self { prior }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => unsafe { std::env::set_var("NORN_HOME", v) },
                None => unsafe { std::env::remove_var("NORN_HOME") },
            }
        }
    }

    /// Whether a pid is still live, via `kill -0` (no unsafe libc call).
    #[cfg(unix)]
    fn process_alive(pid: i64) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|s| s.success())
    }

    /// The shared arming installs the estimator and the context-edit
    /// tracker on the loop context and fills an unset window from the
    /// catalog for the resolved model, leaving the reserve default
    /// untouched. This is the exact end state every launch path (root,
    /// spawn, fork, rhai) must produce — the single mechanism they all
    /// call, so the auto-compaction trigger cannot drift between them.
    #[test]
    fn arm_auto_compaction_installs_estimator_edits_and_catalog_window() {
        let model = crate::model_catalog::default_selection().model;
        let catalog_window = crate::model_catalog::smallest_context_window_for_model(model);
        assert!(
            catalog_window.is_some(),
            "test precondition: the default model must be catalogued",
        );

        let mut loop_context = LoopContext::new("base");
        let mut config = AgentLoopConfig::default();
        assert!(loop_context.token_estimator.is_none());
        assert!(loop_context.context_edits.is_none());
        assert!(config.context_window_limit.is_none());

        arm_auto_compaction(&mut loop_context, &mut config, model);

        assert!(
            loop_context.token_estimator.is_some(),
            "arming installs the token estimator the preflight needs",
        );
        assert!(
            loop_context.context_edits.is_some(),
            "arming installs the context-edit tracker (floor + compaction commit)",
        );
        assert_eq!(
            config.context_window_limit, catalog_window,
            "an unset window is filled from the catalog for the resolved model",
        );
        assert_eq!(
            config.auto_compact_reserve_tokens,
            Some(30_000),
            "the reserve default flows through untouched by arming",
        );
    }

    /// An explicit window (settings / `-c` override / any future
    /// child-policy field) is authoritative: arming only fills a `None`
    /// window, so an explicit value survives even for a catalogued model.
    #[test]
    fn arm_auto_compaction_explicit_window_beats_catalog() {
        let model = crate::model_catalog::default_selection().model;
        let mut loop_context = LoopContext::new("base");
        let mut config = AgentLoopConfig {
            context_window_limit: Some(12_345),
            ..AgentLoopConfig::default()
        };

        arm_auto_compaction(&mut loop_context, &mut config, model);

        assert_eq!(
            config.context_window_limit,
            Some(12_345),
            "an explicit window must never be overwritten by the catalog value",
        );
        assert!(loop_context.token_estimator.is_some());
        assert!(loop_context.context_edits.is_some());
    }

    /// A model absent from the catalog keeps a `None` window — the trigger
    /// stays disabled (`maybe_auto_compact` returns early on `None`),
    /// matching the root behavior, with no error. The estimator and the
    /// tracker are still installed (harmless with the trigger off).
    #[test]
    fn arm_auto_compaction_non_catalog_model_leaves_window_none() {
        let mut loop_context = LoopContext::new("base");
        let mut config = AgentLoopConfig::default();

        arm_auto_compaction(&mut loop_context, &mut config, "not-in-catalog-model-xyz");

        assert_eq!(
            config.context_window_limit, None,
            "a non-catalog model leaves the window None, disabling the trigger",
        );
        assert!(loop_context.token_estimator.is_some());
        assert!(loop_context.context_edits.is_some());
    }

    /// 2026-07-05 incident repro: a global 272k settings override on
    /// gpt-5.3-codex-spark (max 128k) armed every threshold beyond the
    /// real wall. Validation must reject it loudly, naming the model and
    /// both numbers.
    #[test]
    fn validate_rejects_explicit_window_above_catalog_max() {
        let config = AgentLoopConfig {
            context_window_limit: Some(272_000),
            ..AgentLoopConfig::default()
        };
        let err = validate_context_window(&config, "gpt-5.3-codex-spark")
            .expect_err("272k on a 128k-max model must be rejected");
        let reason = err.to_string();
        assert!(
            reason.contains("gpt-5.3-codex-spark"),
            "names the model: {reason}"
        );
        assert!(
            reason.contains("272000"),
            "names the configured value: {reason}"
        );
        assert!(reason.contains("128000"), "names the catalog max: {reason}");
    }

    /// An explicit window at or below the model's catalogued maximum is
    /// legitimate — including a max-window override above the standard
    /// window on models that support one (validation is against
    /// `max_context_window`, not `context_window`).
    #[test]
    fn validate_accepts_windows_up_to_catalog_max() {
        let exact = AgentLoopConfig {
            context_window_limit: Some(128_000),
            ..AgentLoopConfig::default()
        };
        validate_context_window(&exact, "gpt-5.3-codex-spark")
            .expect("a window equal to the model max is valid");

        let model = crate::model_catalog::default_selection().model;
        let max = crate::model_catalog::largest_max_context_window_for_model(model)
            .expect("test precondition: default model is catalogued");
        let at_max = AgentLoopConfig {
            context_window_limit: Some(max),
            ..AgentLoopConfig::default()
        };
        validate_context_window(&at_max, model).expect("catalog max is valid");
    }

    /// The catalog fill composed with validation: a catalogued model with
    /// no explicit window arms from the catalog and validates clean — the
    /// zero-config path every CLI run takes.
    #[test]
    fn validate_accepts_catalog_filled_window() {
        let mut loop_context = LoopContext::new("base");
        let mut config = AgentLoopConfig::default();
        arm_auto_compaction(&mut loop_context, &mut config, "gpt-5.3-codex-spark");
        assert_eq!(config.context_window_limit, Some(128_000));
        validate_context_window(&config, "gpt-5.3-codex-spark")
            .expect("catalog-filled window validates");
    }

    /// Owner ruling (Tom, 2026-07-05): an unknown model "probably means
    /// the wrong model code" — running with protections silently disabled
    /// is the ruled-against state, so a None window after the fill is a
    /// hard error that leads with the typo hypothesis.
    #[test]
    fn validate_rejects_non_catalog_model_without_explicit_window() {
        let config = AgentLoopConfig::default();
        let err = validate_context_window(&config, "not-in-catalog-model-xyz")
            .expect_err("unknown model with no window must be rejected");
        let reason = err.to_string();
        assert!(
            reason.contains("not-in-catalog-model-xyz"),
            "names the model: {reason}"
        );
        assert!(
            reason.contains("typo"),
            "leads with the typo hypothesis: {reason}"
        );
        assert!(
            reason.contains("agent.context_window"),
            "names the config keys that fix it: {reason}",
        );
    }

    /// A deliberate uncatalogued model with an explicit window is
    /// legitimate (local/openai-compatible ids); validation has no
    /// catalog ceiling to check it against and passes it through.
    #[test]
    fn validate_accepts_non_catalog_model_with_explicit_window() {
        let config = AgentLoopConfig {
            context_window_limit: Some(32_000),
            ..AgentLoopConfig::default()
        };
        validate_context_window(&config, "not-in-catalog-model-xyz")
            .expect("explicit window on an uncatalogued model is valid");
    }

    /// The child-path window guard: a catalogued model fills from the
    /// catalog and validates clean; an uncatalogued model (a child has no
    /// explicit-window escape hatch) is rejected loudly, mirroring the
    /// root's unknown-model rejection.
    #[test]
    fn arm_child_window_fills_catalog_model_and_rejects_unknown() {
        let model = crate::model_catalog::default_selection().model;
        let mut config = AgentLoopConfig::default();
        arm_child_window(&mut config, model).expect("catalogued child model validates");
        assert_eq!(
            config.context_window_limit,
            crate::model_catalog::smallest_context_window_for_model(model),
            "the child's window is filled from the catalog for its own model",
        );

        let mut unknown = AgentLoopConfig::default();
        let err = arm_child_window(&mut unknown, "not-in-catalog-model-xyz")
            .expect_err("an uncatalogued child model must be rejected");
        assert!(
            err.to_string().contains("not-in-catalog-model-xyz"),
            "the rejection names the model: {err}",
        );
    }

    /// The child rejection prescribes CHILD remedies only: a catalogued
    /// model or an explicit spawn-time `model` that is catalogued. The
    /// root-only knobs (`agent.context_window` settings, the `-c`
    /// override, the builder window) do not exist on the child path and
    /// must not be prescribed — children have no explicit-window input.
    #[test]
    fn arm_child_window_rejection_prescribes_child_remedies_not_root_knobs() {
        let mut config = AgentLoopConfig::default();
        let err = arm_child_window(&mut config, "not-in-catalog-model-xyz")
            .expect_err("an uncatalogued child model without an override must be rejected");
        let reason = err.to_string();
        assert!(
            reason.contains("child model 'not-in-catalog-model-xyz'"),
            "names the child's model: {reason}",
        );
        assert!(
            reason.contains("typo"),
            "leads with the typo hypothesis: {reason}"
        );
        assert!(
            reason.contains("child_policy.loop_config.context_window"),
            "names the ruled child override (owner ruling 2026-07-07): {reason}",
        );
        for root_only in ["agent.context_window", "-c ", "builder"] {
            assert!(
                !reason.contains(root_only),
                "must not prescribe the root-only remedy '{root_only}': {reason}",
            );
        }
    }

    /// Owner ruling 2026-07-07: an explicit
    /// `child_policy.loop_config.context_window` override on a deliberate
    /// uncatalogued child model is accepted, with exactly that window
    /// armed (mirroring the root's explicit-window semantics).
    #[test]
    fn arm_child_window_accepts_explicit_override_on_uncatalogued_model() {
        let mut config = AgentLoopConfig {
            context_window_limit: Some(32_000),
            ..AgentLoopConfig::default()
        };
        arm_child_window(&mut config, "not-in-catalog-model-xyz")
            .expect("explicit child window on an uncatalogued model is valid");
        assert_eq!(
            config.context_window_limit,
            Some(32_000),
            "the override is armed verbatim, never replaced by a catalog value",
        );
    }

    /// Owner ruling 2026-07-07 + the 2026-07-05 incident guard on the
    /// child path: an explicit child window above a catalogued model's
    /// maximum is rejected loudly (never a silent clamp), naming the
    /// model, both numbers, and the child knob — not the root's.
    #[test]
    fn arm_child_window_rejects_oversized_explicit_override() {
        let mut config = AgentLoopConfig {
            context_window_limit: Some(272_000),
            ..AgentLoopConfig::default()
        };
        let err = arm_child_window(&mut config, "gpt-5.3-codex-spark")
            .expect_err("272k on a 128k-max child model must be rejected");
        let reason = err.to_string();
        assert!(
            reason.contains("gpt-5.3-codex-spark"),
            "names the model: {reason}"
        );
        assert!(
            reason.contains("272000"),
            "names the configured value: {reason}"
        );
        assert!(reason.contains("128000"), "names the catalog max: {reason}");
        assert!(
            reason.contains("child_policy.loop_config.context_window"),
            "names the child knob: {reason}",
        );
        assert!(
            !reason.contains("agent.context_window"),
            "must not prescribe the root-only settings knob: {reason}",
        );
    }

    /// An explicit child window at or below a catalogued model's maximum
    /// beats the catalog fill — explicit config always wins.
    #[test]
    fn arm_child_window_explicit_override_beats_catalog_fill() {
        let mut config = AgentLoopConfig {
            context_window_limit: Some(64_000),
            ..AgentLoopConfig::default()
        };
        arm_child_window(&mut config, "gpt-5.3-codex-spark")
            .expect("an in-range explicit child window is valid");
        assert_eq!(config.context_window_limit, Some(64_000));
    }

    /// Re-review R2: a supported effort passes through unchanged, and no
    /// effort at all stays none — for any source.
    #[test]
    fn arm_child_reasoning_effort_passes_supported_and_none_through() {
        use crate::provider::request::ReasoningEffort;
        let model = crate::model_catalog::default_selection().model;
        assert_eq!(
            arm_child_reasoning_effort(
                Some(ReasoningEffort::High),
                &ChildEffortSource::Explicit("variants.scout.reasoning_effort"),
                model,
            )
            .expect("supported effort is accepted"),
            Some(ReasoningEffort::High),
        );
        assert_eq!(
            arm_child_reasoning_effort(
                Some(ReasoningEffort::High),
                &ChildEffortSource::Inherited { child: "worker" },
                model,
            )
            .expect("supported inherited effort is accepted"),
            Some(ReasoningEffort::High),
        );
        assert_eq!(
            arm_child_reasoning_effort(
                None,
                &ChildEffortSource::Inherited { child: "worker" },
                "not-in-catalog-model-xyz",
            )
            .expect("no effort is always fine"),
            None,
        );
    }

    /// Re-review R2: an EXPLICITLY configured effort the child's resolved
    /// model does not support is a typed error naming the setting and the
    /// model's catalogued efforts — root `/effort` parity, including the
    /// uncatalogued-model case (the root refuses an explicit effort on a
    /// model the catalog cannot vouch for; so does the child path).
    #[test]
    fn arm_child_reasoning_effort_explicit_unsupported_is_a_typed_error() {
        use crate::provider::request::ReasoningEffort;

        // Catalogued model, unsupported effort ("none" is declared for no
        // catalogued model — factual catalog content, not an invention).
        let model = crate::model_catalog::default_selection().model;
        let err = arm_child_reasoning_effort(
            Some(ReasoningEffort::None),
            &ChildEffortSource::Explicit("variants.scout.reasoning_effort"),
            model,
        )
        .expect_err("an unsupported explicit effort must be refused");
        let reason = err.to_string();
        assert!(
            reason.contains("variants.scout.reasoning_effort"),
            "names the setting: {reason}",
        );
        assert!(reason.contains(model), "names the model: {reason}");
        assert!(
            reason.contains("low, medium, high, xhigh"),
            "lists the model's catalogued efforts: {reason}",
        );

        // Uncatalogued model: explicit effort refused, root parity.
        let err = arm_child_reasoning_effort(
            Some(ReasoningEffort::High),
            &ChildEffortSource::Explicit("variants.scout.reasoning_effort"),
            "not-in-catalog-model-xyz",
        )
        .expect_err("an explicit effort on an uncatalogued model must be refused");
        let reason = err.to_string();
        assert!(
            reason.contains("not in the model catalog"),
            "states why no effort can be vouched for: {reason}",
        );
        assert!(
            reason.contains("variants.scout.reasoning_effort"),
            "names the setting: {reason}",
        );
    }

    /// Re-review R2: an INHERITED effort the child's resolved model does
    /// not support degrades to `None` with a `tracing::warn!` naming the
    /// child, the model, and the dropped effort — never an error (the
    /// caller configured nothing wrong on this spawn), never silent.
    #[test]
    fn arm_child_reasoning_effort_inherited_unsupported_warns_and_degrades() {
        use std::sync::{Arc, Mutex};

        use crate::provider::request::ReasoningEffort;

        #[derive(Clone, Default)]
        struct SharedBuf(Arc<Mutex<Vec<u8>>>);

        impl std::io::Write for SharedBuf {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().expect("buffer lock").write(buf)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for SharedBuf {
            type Writer = SharedBuf;
            fn make_writer(&'writer self) -> Self::Writer {
                self.clone()
            }
        }

        let buf = SharedBuf::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_writer(buf.clone())
            .with_ansi(false)
            .finish();

        let degraded = tracing::subscriber::with_default(subscriber, || {
            arm_child_reasoning_effort(
                Some(ReasoningEffort::XHigh),
                &ChildEffortSource::Inherited { child: "explorer" },
                "not-in-catalog-model-xyz",
            )
        })
        .expect("inherited-unsupported must not error");
        assert_eq!(
            degraded, None,
            "the unsupported inherited effort is dropped"
        );

        let output = String::from_utf8(buf.0.lock().expect("buffer lock").clone())
            .expect("log output is UTF-8");
        assert!(output.contains("WARN"), "logs at warn: {output}");
        assert!(
            output.contains("child=explorer"),
            "names the child: {output}"
        );
        assert!(
            output.contains("model=not-in-catalog-model-xyz"),
            "names the model: {output}",
        );
        assert!(
            output.contains("effort=xhigh"),
            "names the dropped effort: {output}",
        );
    }

    /// NP-001 R9: arming installs the `ProcessManager` extension (the `process`
    /// tool resolves it) and binds its shutdown guard to the loop context, for
    /// any agent — root or child — that goes through this shared mechanism.
    #[test]
    fn arm_process_manager_installs_extension_and_shutdown_guard() {
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SessionId("sess".to_owned())));
        let mut loop_context = LoopContext::new("base");
        loop_context.pending_agent_messages = Some(Arc::new(PendingAgentMessages::new()));
        let event_store = Arc::new(EventStore::new());
        let agent_id = Uuid::new_v4();

        assert!(ctx.get_extension::<ProcessManager>().is_none());
        arm_process_manager(&ctx, &mut loop_context, &event_store, agent_id, None, None);

        assert!(
            ctx.get_extension::<ProcessManager>().is_some(),
            "the process tool's manager extension is installed",
        );
        assert!(
            loop_context.process_manager.is_some(),
            "the shutdown guard is bound to the loop context",
        );
    }

    /// R9 / F4: dropping the `LoopContext` (which owns the
    /// `ProcessManagerGuard`) runs the manager's shutdown at the runtime-drop
    /// level — a still-running process group is killed, so an OS pid probe of a
    /// backgrounded grandchild fails afterwards. This proves teardown through
    /// the real arming path and guard drop, not merely a direct
    /// `ProcessManager::shutdown` call.
    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial]
    async fn dropping_the_loop_context_kills_running_process_groups() {
        let dir = tempfile::tempdir().unwrap();
        let _home = HomeGuard::set(dir.path());
        let gc_file = dir.path().join("grandchild.pid");
        let ctx = ToolContext::empty();
        ctx.insert_extension(Arc::new(SessionId("sess".to_owned())));
        let mut loop_context = LoopContext::new("base");
        loop_context.pending_agent_messages = Some(Arc::new(PendingAgentMessages::new()));
        let event_store = Arc::new(EventStore::new());
        let agent_id = Uuid::new_v4();

        arm_process_manager(&ctx, &mut loop_context, &event_store, agent_id, None, None);
        let manager = ctx
            .get_extension::<ProcessManager>()
            .expect("manager armed on the tool context");
        let cwd = std::env::current_dir().unwrap();
        let handle = manager
            .spawn(
                &format!("sleep 300 & echo $! > '{}'; sleep 300", gc_file.display()),
                &cwd,
                None,
            )
            .await
            .unwrap();

        // Probe the backgrounded grandchild (it shares the process group): its
        // parent is the shell, so after the group kill init reaps it and
        // `kill -0` then fails. The shell child itself would linger as a zombie
        // (the aborted supervisor never reaps it), so it is not a reliable probe.
        let gc_pid: i64 = {
            let mut found = None;
            for _ in 0..600 {
                if let Ok(text) = std::fs::read_to_string(&gc_file)
                    && let Ok(pid) = text.trim().parse()
                {
                    found = Some(pid);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            found.expect("grandchild pid recorded")
        };
        assert!(process_alive(gc_pid), "the grandchild is alive after spawn");
        let _ = handle;

        // Drop the loop context: its ProcessManagerGuard shuts the manager down
        // and kills the still-running group — even though the manager Arc still
        // lingers on the tool context.
        drop(loop_context);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while process_alive(gc_pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "grandchild (pid {gc_pid}) survived the loop-context drop",
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // The manager Arc lingered, but the guard drop already ran shutdown.
        drop(manager);
    }

    /// A child's skill listing is gated on the `skill` tool being on the
    /// child's resolved surface: present + admitted → available; present +
    /// excluded by allow-list → unavailable; absent registry → unavailable.
    #[test]
    fn child_skill_tool_available_respects_registry_and_allow_list() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(crate::tools::skill::SkillTool::new()));

        assert!(child_skill_tool_available(&registry, None));
        assert!(child_skill_tool_available(
            &registry,
            Some(&["skill".to_owned(), "read".to_owned()]),
        ));
        assert!(!child_skill_tool_available(
            &registry,
            Some(&["read".to_owned()])
        ));

        let empty = ToolRegistry::new();
        assert!(!child_skill_tool_available(&empty, None));
    }

    /// The shared child-listing installer folds the "# Available Skills"
    /// section into the child's base instruction (after the base) when the
    /// skill tool is available, and leaves the instruction untouched when it
    /// is not — the same filtered listing the root gets.
    #[test]
    fn install_child_skill_listing_appends_when_available_and_skips_when_not() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("greet");
        std::fs::create_dir(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: greet the user\n---\nbody",
        )
        .unwrap();
        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);

        let mut available = LoopContext::new("You are a sub-agent.");
        install_child_skill_listing(&mut available, &catalog, true);
        let base = available.base_system_instruction();
        assert!(
            base.contains("You are a sub-agent."),
            "base retained: {base}"
        );
        assert!(
            base.contains("# Available Skills"),
            "listing present when available: {base}",
        );
        assert!(
            base.find("You are a sub-agent.") < base.find("# Available Skills"),
            "the base instruction must precede the listing: {base}",
        );

        let mut gated = LoopContext::new("You are a sub-agent.");
        install_child_skill_listing(&mut gated, &catalog, false);
        assert_eq!(
            gated.base_system_instruction(),
            "You are a sub-agent.",
            "an unavailable skill tool leaves the child's instruction untouched",
        );
    }

    /// Regression: an embedder-supplied parent base
    /// (`ParentSystemInstruction`) may already contain the listing — the
    /// root's `base_system_instruction()` includes its materialized
    /// `base_suffix`. Installing on such a base must not duplicate the
    /// "# Available Skills" section.
    #[test]
    fn install_child_skill_listing_does_not_duplicate_listing_bearing_base() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("greet");
        std::fs::create_dir(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ndescription: greet the user\n---\nbody",
        )
        .unwrap();
        let catalog = SkillCatalog::scan(&[dir.path().to_path_buf()]);

        // A parent base that already carries the exact generated listing,
        // as a root's materialized instruction would.
        let listing_bearing_base =
            format!("You are the parent.\n\n{}", catalog.system_prompt_listing());
        let mut child = LoopContext::new(&listing_bearing_base);
        install_child_skill_listing(&mut child, &catalog, true);

        let base = child.base_system_instruction();
        assert_eq!(
            base.matches("# Available Skills").count(),
            1,
            "the listing must appear exactly once: {base}",
        );
        assert_eq!(
            base, listing_bearing_base,
            "a listing-bearing base is left untouched",
        );

        // Idempotency of the guard itself: a second install is also a no-op.
        install_child_skill_listing(&mut child, &catalog, true);
        assert_eq!(
            child
                .base_system_instruction()
                .matches("# Available Skills")
                .count(),
            1,
            "repeat installs must not duplicate the section",
        );
    }
}
