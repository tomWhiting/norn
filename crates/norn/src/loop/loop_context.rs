//! Loop-wide context bundling optional components that the agent loop
//! consults during execution: rules engine, hook registry, mutable system
//! instruction sections, per-event schemas, the iteration monitor, profile
//! reasoning effort, registered slash commands, prompt commands, and
//! production-hardening hooks (retry policy, token estimator, session
//! variables, and context edits for auto-compaction).
//!
//! The agent loop accepts a `&mut LoopContext` instead of an exploding list
//! of optional parameters. Components default to absent so callers that do
//! not need rules or hooks can simply construct
//! [`LoopContext::new`]`(system_instruction)` and pass it through.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::context::loader::ContextLoader;
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::HookRegistry;
use crate::system_prompt::builder::CollaborationMode;
use crate::system_prompt::environment::EnvironmentConfig;
use crate::system_prompt::plan::PromptPlan;

use crate::integration::variables::VariableStore;
use crate::r#loop::commands::SlashCommandRegistry;
use crate::r#loop::event_schemas::EventSchemaSet;
use crate::r#loop::iteration::IterationMonitorConfig;

use crate::r#loop::retry::RetryPolicy;

use crate::r#loop::tokens::TokenEstimator;
use crate::profile::PromptCommand;
use crate::provider::request::{ReasoningEffort, ReasoningSummary, ServiceTier};
use crate::rules::engine::RuleEngine;

use crate::session::action_log::ActionLog;
use crate::session::context_edit::ContextEdits;

mod prompt_context;

/// Default wall-clock budget for [`PromptCommand`] execution. Mirrors the
/// shell-variable timeout in `integration::variables` so the runtime has a
/// single predictable bound on synchronous shell work at prompt-construction
/// time.
pub const DEFAULT_PROMPT_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// Cached output of a single [`PromptCommand`] resolve.
///
/// Each cache entry binds the exact command text, configured TTL, and working
/// directory to the verbatim stdout (already trimmed of trailing newlines).
/// Commands without a TTL bypass the cache entirely; production entries
/// therefore always carry an absolute expiry deadline.
#[derive(Clone, Debug)]
pub struct PromptCommandCacheEntry {
    /// Exact shell command whose output this entry contains.
    pub command: String,
    /// Configured TTL that produced this entry.
    pub cache_ttl: Duration,
    /// Working directory in which the command produced this output.
    pub working_dir: std::path::PathBuf,
    /// The trimmed stdout produced by the command.
    pub value: String,
    /// Absolute expiry deadline. Production entries are always `Some` because
    /// commands without a TTL are never cached.
    pub expires_at: Option<Instant>,
}

/// Optional bundle of loop-wide components plus typed stable and volatile
/// prompt state.
///
/// The authoritative stable source plan is retained separately; its flattened
/// compatibility view is the first entry in [`Self::system_sections`].
/// Volatile sections appended via [`Self::append_system_section`] are rebuilt
/// each iteration. Threaded Responses sends them as request-local System
/// instructions, while stateless transports append one Developer tail.
/// [`Self::clear_dynamic_sections`] truncates only those volatile sections.
pub struct LoopContext {
    /// Optional rules engine. When present, the loop emits
    /// [`RuntimeEvent`](crate::rules::types::RuntimeEvent) values after tool
    /// execution and applies the engine's
    /// [`RuleInjection`](crate::rules::types::RuleInjection) results.
    pub rules: Option<RuleEngine>,
    /// Reactive scanner that registers nested `NORN.md` files as synthetic
    /// rules on [`Self::rules`] the first time a path under their directory
    /// is touched (NX-004). Assembly captures its immutable workspace root
    /// before the live working-directory handle can change. Public
    /// constructors retain the scanner even before a rules engine is attached,
    /// preserving support for embedders that install rules after construction.
    pub nested_scanner: Option<crate::context::scanner::NestedScanner>,
    /// Optional hook registry. When present, the loop calls pre/post tool,
    /// pre/post LLM, and session-event hooks at their respective firing
    /// points.
    ///
    /// Wrapped in [`Arc`] so the registry can also be cloned onto a
    /// [`ToolContext`](crate::tool::context::ToolContext) extension for
    /// sub-agent dispatch sites (notably `tools/agent/spawn.rs`) that do
    /// not hold a [`LoopContext`] reference.
    pub hooks: Option<Arc<HookRegistry>>,
    /// Composable compatibility prompt sections. Index 0 is the flattened
    /// stable plan; later entries are volatile managed sections cleared at the
    /// start of each iteration. Sourced rule injections travel as durable
    /// Developer/User messages rather than entries in this vector.
    pub system_sections: Vec<String>,
    /// Volatile trusted-operator sections that retain Developer authority.
    /// Threaded Responses binds the current joined value into its prompt seed;
    /// stateless transports fold it into the managed Developer tail.
    pub(crate) developer_sections: Vec<String>,
    /// Optional per-event output schemas. When present, the loop validates
    /// the corresponding event types before recording them.
    pub event_schemas: Option<EventSchemaSet>,
    /// Optional iteration monitor configuration. When present, the loop
    /// evaluates token-budget, repeated-failure, and quality signals each
    /// iteration.
    pub iteration_monitor: Option<IterationMonitorConfig>,
    /// Optional reasoning-effort hint threaded into every
    /// [`ProviderRequest`](crate::provider::request::ProviderRequest).
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Optional reasoning-summary verbosity threaded into every
    /// [`ProviderRequest`](crate::provider::request::ProviderRequest).
    pub reasoning_summary: Option<ReasoningSummary>,
    /// Optional service tier threaded into every
    /// [`ProviderRequest`](crate::provider::request::ProviderRequest).
    pub service_tier: Option<ServiceTier>,
    /// Optional registry of slash commands. When present, user input is
    /// pre-processed through [`crate::agent_loop::commands::preprocess_input`]
    /// before reaching the model.
    pub slash_commands: Option<SlashCommandRegistry>,
    /// Profile-supplied shell commands evaluated at the start of every
    /// iteration; their stdout populates volatile Developer sections.
    pub prompt_commands: Vec<PromptCommand>,
    /// Cache of prior prompt-command results keyed by command name. Entries
    /// only exist for commands with a `cache_ttl`. Commands without a TTL
    /// always re-execute.
    pub prompt_command_cache: HashMap<String, PromptCommandCacheEntry>,

    /// Retry policy applied to every provider call site. Transient errors
    /// (network timeouts, connection resets, 5xx, rate limits) are retried
    /// with exponential backoff. Always active — a headless runtime must
    /// not die from a single network hiccup.
    pub retry_policy: RetryPolicy,

    /// Optional client-side token estimator used to plan context window

    /// usage before each provider call. Boxed for trait-object dispatch and

    /// shared via [`Arc`] so callers can reuse a single estimator across

    /// agents without cloning the implementation.
    pub token_estimator: Option<Arc<dyn TokenEstimator>>,

    /// Optional session variable store. When present, the loop expands

    /// `{{name}}` placeholders in the system instruction and tool

    /// descriptions before each provider call. Shared via [`Arc`] so the

    /// same store can be threaded through several agents.
    pub variables: Option<Arc<VariableStore>>,

    /// Optional context-edits tracker used by auto-compaction. When

    /// present alongside a token estimator and the

    /// [`AgentLoopConfig::auto_compact_reserve_tokens`](crate::agent_loop::runner::AgentLoopConfig)

    /// trigger, the loop appends a

    /// [`SessionEvent::Compaction`](crate::session::events::SessionEvent::Compaction)

    /// once estimated usage crosses `context_window_limit − reserve`.
    pub context_edits: Option<ContextEdits>,

    /// Whether persisted context-edit marks (compaction supersession,
    /// suppress, inject) have been loaded into [`Self::context_edits`].
    ///
    /// The runner loads persisted marks **once per loop context** — its
    /// first step walks the store (see `run_agent_step_inner`) — covering
    /// drivers that resume a session with a fresh [`ContextEdits`]
    /// without going through the library's resume path. Every mark
    /// applied after that point lands on the tracker at apply time
    /// ([`ContextEdits::suppress`], [`ContextEdits::inject`],
    /// [`ContextEdits::summarize`], [`ContextEdits::compact`],
    /// [`ContextEdits::commit_compaction_plan`]), so no per-step store
    /// re-walk exists. Drivers that pre-load marks (resume via
    /// [`ReplayArtifacts`](crate::session::ReplayArtifacts) plus the
    /// [`ContextEdits::mark_superseded`] / `mark_suppressed` /
    /// `mark_injected` restorers) may set this to `true` to skip the
    /// runner's one-time walk; leaving it `false` costs exactly one
    /// idempotent walk on the first step.
    pub context_marks_loaded: bool,

    /// Optional diagnostic collector. When present, the runner pushes a
    /// [`NornDiagnostic`](crate::integration::NornDiagnostic) at three sites
    /// — schema validation failure, pre-validate block, and post-validate
    /// failure — and the same collector is published on
    /// [`ToolContext`](crate::tool::context::ToolContext) so runtime
    /// post-validate checks can push directly. When `None`, no collection
    /// occurs and the loop behaves identically to before.
    pub diagnostics: Option<Arc<DiagnosticCollector>>,

    /// Optional action log indexing every completed tool dispatch.
    ///
    /// When present, the tool dispatch path calls
    /// [`ActionLog::record_completion`](crate::session::action_log::ActionLog::record_completion)
    /// after each call (success, error, or hook-blocked) so the agent
    /// can later drill back into prior calls regardless of context
    /// compaction state. When `None`, no recording occurs and the loop
    /// behaves identically to before.
    ///
    /// The same [`EventStore`](crate::session::store::EventStore) that
    /// the loop appends [`SessionEvent::ToolResult`](crate::session::events::SessionEvent::ToolResult)
    /// events into must be the one the [`ActionLog`] was constructed
    /// with — otherwise `get_detail` and `get_context` cannot locate
    /// the matching events.
    pub action_log: Option<Arc<ActionLog>>,

    /// Current agent id for delivery of durable queued inter-agent messages.
    ///
    /// Runtime assembly stamps this for roots, spawned children, and forks
    /// when agent coordination is installed. Paired with
    /// [`Self::pending_agent_messages`]; both must be present before the
    /// runner drains queued messages into this loop.
    pub agent_id: Option<Uuid>,

    /// Shared pending-message store for this agent tree.
    ///
    /// `signal_agent` writes here when a resolved, in-scope recipient has no
    /// live router route but can still have a future consumer. The runner
    /// drains the current agent's queue at step start and injects those
    /// messages through the normal `<agent_message>` path. Coordination-aware
    /// embedders should install this through [`Self::install_pending_mailbox`]
    /// instead of assigning the field directly, so the durable store, mailbox
    /// identity, and controller lifetime remain one validated unit.
    pub pending_agent_messages: Option<Arc<crate::agent::PendingAgentMessages>>,

    /// Root-controller proof keeping its durable mailbox registration live.
    /// The mailbox registry itself stores only a weak reference, so a root
    /// that is dropped cannot remain an apparently deliverable recipient just
    /// because another handle still retains its event store.
    pub(crate) pending_mailbox_lease: Option<Arc<crate::agent::PendingMailboxLease>>,

    /// Guard binding the agent's in-session schedule executor
    /// ([`crate::schedule::executor`]) to this loop's lifetime.
    ///
    /// Set by every assembly launch path that arms scheduling (root build,
    /// spawn, fork). Dropping the loop context — the agent instance for a
    /// root, the controller task for a child — aborts the executor task, so
    /// no timer outlives its agent. Inert data as far as the runner is
    /// concerned; nothing in the loop reads it.
    pub schedule_executor: Option<crate::schedule::ScheduleExecutorGuard>,

    /// Guard binding the agent's background-process manager
    /// ([`crate::process::ProcessManager`]) to this loop's lifetime.
    ///
    /// Set by every assembly launch path that arms the manager (root build,
    /// spawn, fork). Dropping the loop context — the agent instance for a root,
    /// the controller task for a child — runs the manager's shutdown: every
    /// still-running process group is killed and its spool left on disk, so a
    /// norn exit never silently orphans a manager-owned child. Inert data as
    /// far as the runner is concerned; nothing in the loop reads it.
    pub process_manager: Option<crate::process::ProcessManagerGuard>,

    /// Optional always-on `NORN.md` context loader. When present,
    /// [`Self::refresh_context_if_stale`] stats both layers per
    /// iteration and reports back whether `system_sections[0]` needs
    /// rebuilding. The rebuild itself is performed via
    /// [`Self::rebuild_base_section`], which the iteration top in the
    /// runner calls when staleness is observed.
    pub context_loader: Option<ContextLoader>,

    /// Source-aware stable prompt plan installed by root assembly.
    ///
    /// `None` preserves the legacy public [`Self::new`] contract: the first
    /// `system_sections` entry is emitted as one System message. Root assembly
    /// installs a plan so product, operator, and repository fragments retain
    /// their distinct authorities without changing that compatibility view.
    pub stable_prompt_plan: Option<PromptPlan>,

    /// Internal flattened compatibility prefix layered into
    /// `system_sections[0]` ahead of the always-on `NORN.md` content.
    ///
    /// Populated by `build_runtime` (NX-005) with the Norn base prompt
    /// produced by `build_system_prompt` concatenated with the profile's
    /// resolved system instructions. Empty by default so the
    /// [`LoopContext::default`] / [`LoopContext::new`] shapes still
    /// produce a single-element `system_sections` containing only the
    /// caller-supplied base.
    ///
    /// Embedders that need source-aware prompt changes install a
    /// [`PromptPlan`] through [`Self::install_stable_prompt_plan`]; this
    /// compatibility cache is deliberately not a public mutation surface.
    pub(crate) base_prefix: String,

    /// Internal flattened compatibility suffix layered into
    /// `system_sections[0]` after the always-on `NORN.md` content.
    ///
    /// Populated by `build_runtime` (NX-005) with the skill catalog
    /// `# Available Skills` listing when at least one skill is
    /// discovered. Empty by default so callers that do not surface a
    /// skill catalog see no trailing separator.
    ///
    /// Embedders must use a source-addressed [`PromptPlan`] rather than
    /// appending untyped text whose authority cannot be derived safely.
    pub(crate) base_suffix: String,

    /// Optional environment configuration for the dynamic `# Environment`
    /// section. When present, [`Self::inject_environment_section`] appends
    /// a dynamic section with current time, working directory, git branch,
    /// and session metadata each iteration.
    pub environment: Option<EnvironmentConfig>,

    /// Collaboration mode governing how the agent approaches its work.
    /// Injected as a dynamic section each iteration via
    /// [`Self::inject_collaboration_mode`]. Changeable mid-session.
    pub collaboration_mode: CollaborationMode,

    /// Receiver for child-agent completion results (fork/spawn).
    /// When present, the runner drains pending results at iteration
    /// boundaries and injects them as user-role messages. This moves
    /// child result delivery into the runner so drivers don't need to
    /// handle it individually.
    pub child_result_rx:
        Option<tokio::sync::mpsc::Receiver<crate::agent::result_channel::ChildAgentResult>>,

    /// Receiver for human input submitted while this turn is already running.
    ///
    /// Unlike [`Self::pending_agent_messages`] and [`Self::child_result_rx`],
    /// this is not inter-agent traffic and is not harness-framed. The runner
    /// drains it at safe provider boundaries and persists each entry as an
    /// ordinary [`SessionEvent::UserMessage`](crate::session::events::SessionEvent::UserMessage)
    /// before the model can see it.
    pub active_input_rx: Option<crate::r#loop::active_input::ActiveInputReceiver>,

    /// Accumulated `subtree_usage` of every child result delivered into
    /// this loop (W3.6 usage rollup).
    ///
    /// [`drain_child_results`](crate::agent_loop) — the single injection path
    /// for child results, mid-run and lingering alike — folds each
    /// drained result's
    /// [`subtree_usage`](crate::agent::result_channel::ChildAgentResult::subtree_usage)
    /// in here, and every [`AgentStepResult`](crate::agent_loop::config::AgentStepResult)
    /// arm carries a snapshot alongside its own-calls-only `usage`, so
    /// the spawn/fork completion wrappers can compute
    /// `subtree_usage = total_usage + children_usage` without reaching
    /// into the loop. Deliberately a shared handle, not a plain value:
    /// the wrapper's clone survives a panicking loop task so delivered
    /// descendant spend is never silently lost (see [`ChildrenUsage`](crate::agent_loop::children_usage::ChildrenUsage)).
    pub children_usage: crate::r#loop::children_usage::ChildrenUsage,

    /// Per-agent working directory shared with [`ToolContext`](crate::tool::context::ToolContext).
    ///
    /// Bash's `cd` parsing updates this; subsequent prompt commands,
    /// shell hooks, rules engine, Rhai `run_cmd`, and shell variable
    /// expansion read it to set the child process's CWD and to start
    /// path resolution from. Cloning this field yields a handle that
    /// shares the same underlying value — orchestrators install one
    /// `SharedWorkingDir` at the entry point and clone it into both
    /// [`crate::tool::context::ToolContext`] and this field.
    pub working_dir: crate::tool::context::SharedWorkingDir,
}

impl Default for LoopContext {
    fn default() -> Self {
        let mut context = Self::new(String::new());
        // Preserve the historical empty-section shape of `Default` while
        // retaining the immutable launch-root scanner seeded by `new`.
        context.system_sections.clear();
        context
    }
}

impl LoopContext {
    /// Construct a fresh loop context with the given base system instruction.
    ///
    /// The working directory defaults to [`std::env::current_dir`] at
    /// construction time. Use [`Self::with_working_dir`] when an orchestrator
    /// already holds a shared handle.
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        let working_dir = crate::tool::context::SharedWorkingDir::default();
        Self {
            rules: None,
            nested_scanner: Some(crate::context::scanner::NestedScanner::new(
                &working_dir.get(),
            )),
            hooks: None,
            system_sections: vec![base.into()],
            developer_sections: Vec::new(),
            event_schemas: None,
            iteration_monitor: None,
            reasoning_effort: None,
            reasoning_summary: None,
            service_tier: None,
            slash_commands: None,
            prompt_commands: Vec::new(),
            prompt_command_cache: HashMap::new(),
            retry_policy: RetryPolicy::default(),
            token_estimator: None,
            variables: None,
            context_edits: None,
            context_marks_loaded: false,
            diagnostics: None,
            action_log: None,
            agent_id: None,
            pending_agent_messages: None,
            pending_mailbox_lease: None,
            schedule_executor: None,
            process_manager: None,
            context_loader: None,
            stable_prompt_plan: None,
            base_prefix: String::new(),
            base_suffix: String::new(),
            environment: None,
            collaboration_mode: CollaborationMode::default(),
            child_result_rx: None,
            active_input_rx: None,
            children_usage: crate::r#loop::children_usage::ChildrenUsage::default(),
            working_dir,
        }
    }

    /// Construct a fresh loop context that shares the given working-dir
    /// handle with the orchestrator's [`crate::tool::context::ToolContext`].
    #[must_use]
    pub fn with_working_dir(
        base: impl Into<String>,
        working_dir: crate::tool::context::SharedWorkingDir,
    ) -> Self {
        let mut ctx = Self::new(base);
        ctx.nested_scanner = Some(crate::context::scanner::NestedScanner::new(
            &working_dir.get(),
        ));
        ctx.working_dir = working_dir;
        ctx
    }

    /// Install the durable pending-message mailbox used by public step runners.
    ///
    /// The mailbox identity comes from the session binding rather than the
    /// runtime agent id, so resume can safely use a fresh runtime id while a
    /// fork cannot claim its parent's queued messages. Existing queue rows are
    /// rebuilt from the exact store before any context field is published.
    /// The retained controller lease keeps the registration live for this
    /// context's lifetime.
    ///
    /// # Errors
    ///
    /// Returns a typed session error when the context already carries a
    /// conflicting coordination identity or the durable queue history is
    /// malformed.
    pub fn install_pending_mailbox(
        &mut self,
        agent_id: Uuid,
        binding: &crate::session::SessionBinding,
        store: &Arc<crate::session::store::EventStore>,
    ) -> Result<Arc<crate::agent::PendingAgentMessages>, crate::error::SessionError> {
        if self.pending_agent_messages.is_some()
            || self.pending_mailbox_lease.is_some()
            || self.agent_id.is_some_and(|current| current != agent_id)
        {
            return Err(crate::error::SessionError::StorageError {
                reason: "loop context already has a different pending-message mailbox".to_owned(),
            });
        }

        let pending = Arc::new(crate::agent::PendingAgentMessages::from_events(
            agent_id,
            binding.mailbox_id(),
            &store.events(),
        )?);
        let lease = Arc::new(crate::agent::PendingMailboxLease::new());
        pending.register_root_mailbox(agent_id, binding.mailbox_id(), store, &lease)?;

        self.agent_id = Some(agent_id);
        self.pending_agent_messages = Some(Arc::clone(&pending));
        self.pending_mailbox_lease = Some(lease);
        Ok(pending)
    }
}

#[cfg(test)]
mod prompt_runtime_tests;
#[cfg(test)]
mod rule_context_tests;
#[cfg(test)]
mod state_tests;
