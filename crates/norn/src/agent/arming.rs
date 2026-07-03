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

use crate::agent::pending_messages::PendingAgentMessages;
use crate::agent::registry::AgentRegistry;
use crate::r#loop::config::AgentLoopConfig;
use crate::r#loop::loop_context::LoopContext;
use crate::r#loop::tokens::SimpleTokenEstimator;
use crate::session::context_edit::ContextEdits;
use crate::session::store::EventStore;
use crate::skill::SkillCatalog;
use crate::tool::context::ToolContext;
use crate::tool::registry::ToolRegistry;

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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

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
