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

use tokio::process::Command;
use uuid::Uuid;

use crate::context::loader::ContextLoader;
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::HookRegistry;
use crate::system_prompt::builder::CollaborationMode;
use crate::system_prompt::environment::{EnvironmentConfig, format_environment_section};

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

/// Default wall-clock budget for [`PromptCommand`] execution. Mirrors the
/// shell-variable timeout in `integration::variables` so the runtime has a
/// single predictable bound on synchronous shell work at prompt-construction
/// time.
pub const DEFAULT_PROMPT_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// Cached output of a single [`PromptCommand`] resolve.
///
/// Each cache entry records the verbatim stdout (already trimmed of trailing
/// newlines) and the absolute expiry deadline. `expires_at == None` means
/// the value never expires from the runtime's point of view — which only
/// happens when `cache_ttl` was set; commands without a TTL bypass the cache
/// entirely.
#[derive(Clone, Debug)]
pub struct PromptCommandCacheEntry {
    /// The trimmed stdout produced by the command.
    pub value: String,
    /// Absolute expiry deadline. `None` when caching is disabled for this
    /// command (no entry is ever stored in that case).
    pub expires_at: Option<Instant>,
}

/// Optional bundle of loop-wide components plus a composable system
/// instruction.
///
/// The base system instruction is the first entry in [`Self::system_sections`].
/// Dynamic sections appended via [`Self::append_system_section`] are joined
/// onto the base instruction every iteration via [`Self::system_instruction`].
/// Calling [`Self::clear_dynamic_sections`] truncates everything past the
/// base instruction so rules re-fire fresh each iteration.
#[derive(Default)]
pub struct LoopContext {
    /// Optional rules engine. When present, the loop emits
    /// [`RuntimeEvent`](crate::rules::types::RuntimeEvent) values after tool
    /// execution and applies the engine's
    /// [`RuleInjection`](crate::rules::types::RuleInjection) results.
    pub rules: Option<RuleEngine>,
    /// Reactive scanner that registers nested `NORN.md` files as synthetic
    /// rules on [`Self::rules`] the first time a path under their directory
    /// is touched (NX-004). Lazily constructed on first use from
    /// [`Self::working_dir`] so no assembly site has to thread the project
    /// root separately; absent when no rules engine is installed.
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
    /// Composable system instruction sections. Index 0 is the base
    /// instruction supplied at construction time; later entries are dynamic
    /// sections appended by rule injections and cleared at the start of
    /// each iteration.
    pub system_sections: Vec<String>,
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
    /// iteration; their stdout populates dynamic system sections.
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

    /// Whether persisted compaction supersession marks have been loaded
    /// into [`Self::context_edits`].
    ///
    /// The runner loads persisted marks **once per loop context** — its
    /// first step walks the store (see `run_agent_step_inner`) — covering
    /// drivers that resume a session with a fresh [`ContextEdits`]
    /// without going through the library's resume path. Every compaction
    /// appended after that point marks supersession at append time on
    /// the tracker itself ([`ContextEdits::summarize`],
    /// [`ContextEdits::compact`],
    /// [`ContextEdits::commit_compaction_plan`]), so no per-step store
    /// re-walk exists. Drivers that pre-load marks (resume via
    /// [`ReplayArtifacts`](crate::session::ReplayArtifacts) plus
    /// [`ContextEdits::mark_superseded`]) may set this to `true` to skip
    /// the runner's one-time walk; leaving it `false` costs exactly one
    /// idempotent walk on the first step.
    pub compaction_marks_loaded: bool,

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
    /// messages through the normal `<agent_message>` path.
    pub pending_agent_messages: Option<Arc<crate::agent::PendingAgentMessages>>,

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

    /// Static prefix layered into `system_sections[0]` ahead of the
    /// always-on `NORN.md` content.
    ///
    /// Populated by `build_runtime` (NX-005) with the Norn base prompt
    /// produced by `build_system_prompt` concatenated with the profile's
    /// resolved system instructions. Empty by default so the
    /// [`LoopContext::default`] / [`LoopContext::new`] shapes still
    /// produce a single-element `system_sections` containing only the
    /// caller-supplied base.
    pub base_prefix: String,

    /// Static suffix layered into `system_sections[0]` after the
    /// always-on `NORN.md` content.
    ///
    /// Populated by `build_runtime` (NX-005) with the skill catalog
    /// `# Available Skills` listing when at least one skill is
    /// discovered. Empty by default so callers that do not surface a
    /// skill catalog see no trailing separator.
    pub base_suffix: String,

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
    /// boundaries and injects them as developer messages. This moves
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

impl LoopContext {
    /// Construct a fresh loop context with the given base system instruction.
    ///
    /// The working directory defaults to [`std::env::current_dir`] at
    /// construction time. Use [`Self::with_working_dir`] when an orchestrator
    /// already holds a shared handle.
    #[must_use]
    pub fn new(base: impl Into<String>) -> Self {
        Self {
            rules: None,
            nested_scanner: None,
            hooks: None,
            system_sections: vec![base.into()],
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
            compaction_marks_loaded: false,
            diagnostics: None,
            action_log: None,
            agent_id: None,
            pending_agent_messages: None,
            schedule_executor: None,
            process_manager: None,
            context_loader: None,
            base_prefix: String::new(),
            base_suffix: String::new(),
            environment: None,
            collaboration_mode: CollaborationMode::default(),
            child_result_rx: None,
            active_input_rx: None,
            children_usage: crate::r#loop::children_usage::ChildrenUsage::default(),
            working_dir: crate::tool::context::SharedWorkingDir::default(),
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
        ctx.working_dir = working_dir;
        ctx
    }

    /// Reassemble the base system instruction at `system_sections[0]`
    /// from [`Self::base_prefix`], the current
    /// [`ContextLoader::formatted_context`] (when a loader is wired),
    /// and [`Self::base_suffix`].
    ///
    /// Order is fixed: prefix, then always-on context, then suffix —
    /// matching DESIGN.md §D2's layering (Norn base + profile
    /// instructions, then user-level + project-root `NORN.md`, then the
    /// skill catalog listing). Empty parts are skipped so the join does
    /// not produce stray blank lines.
    ///
    /// Pushes a new entry when `system_sections` is empty so callers
    /// can invoke this on a freshly-defaulted [`LoopContext`] without a
    /// separate seeding step.
    pub fn rebuild_base_section(&mut self) {
        let context_body = self
            .context_loader
            .as_ref()
            .map(ContextLoader::formatted_context)
            .unwrap_or_default();
        let parts: [&str; 3] = [
            self.base_prefix.as_str(),
            context_body.as_str(),
            self.base_suffix.as_str(),
        ];
        let assembled: String = parts
            .iter()
            .copied()
            .filter(|s| !s.is_empty())
            .collect::<Vec<&str>>()
            .join("\n\n");
        if let Some(slot) = self.system_sections.first_mut() {
            *slot = assembled;
        } else {
            self.system_sections.push(assembled);
        }
    }

    /// Re-stat the always-on context files and report whether
    /// `system_sections[0]` needs rebuilding.
    ///
    /// Returns `false` when no loader is wired (so callers can invoke
    /// this unconditionally from the iteration top) or when both layers
    /// are unchanged since the last call. Returns `true` when at least
    /// one always-on `NORN.md` layer was added, removed, or rewritten on
    /// disk — at which point the caller (NX-005's iteration wiring)
    /// rebuilds the base instruction from the freshly-loaded
    /// [`ContextLoader::formatted_context`].
    ///
    /// Designed to be called between
    /// [`Self::clear_dynamic_sections`] and
    /// [`Self::evaluate_prompt_commands`] at the start of each iteration
    /// (NX-005 wires the call site in `loop/runner.rs`; this brief only
    /// supplies the method).
    pub fn refresh_context_if_stale(&mut self) -> bool {
        match self.context_loader.as_mut() {
            Some(loader) => loader.check_staleness(),
            None => false,
        }
    }

    /// Join all system sections with a double newline, producing the system
    /// instruction string for the next provider call.
    #[must_use]
    pub fn system_instruction(&self) -> String {
        self.system_sections.join("\n\n")
    }

    /// Return only the base system instruction (index 0), without any
    /// dynamic sections. The base prompt is byte-stable between iterations,
    /// enabling automatic prefix caching on the provider's instructions
    /// field.
    #[must_use]
    pub fn base_system_instruction(&self) -> String {
        self.system_sections.first().cloned().unwrap_or_default()
    }

    /// Collect dynamic sections (indices 1..) into a single string joined
    /// by double newlines. Returns [`None`] when no dynamic sections exist.
    #[must_use]
    pub fn dynamic_context(&self) -> Option<String> {
        if self.system_sections.len() <= 1 {
            return None;
        }
        Some(self.system_sections[1..].join("\n\n"))
    }

    /// Append a dynamic section to the system instruction.
    ///
    /// Dynamic sections live past index 0 and are cleared at the start of
    /// each loop iteration via [`Self::clear_dynamic_sections`].
    pub fn append_system_section(&mut self, content: impl Into<String>) {
        self.system_sections.push(content.into());
    }

    /// Drop all dynamic sections, retaining only the base instruction at
    /// index 0. Called at the top of each loop iteration so rule injections
    /// re-fire fresh.
    pub fn clear_dynamic_sections(&mut self) {
        self.system_sections.truncate(1);
    }

    /// Build the current prompt view over `store`, honouring the active
    /// [`ContextEdits`] when one is installed. When no edits tracker is
    /// present nothing is suppressed, so the view is every stored event.
    /// Re-append every in-context [`DeliveryMode::SystemContextAppend`] rule's
    /// content to the dynamic system sections from the persisted event
    /// stream.
    ///
    /// Called at the top of each iteration after
    /// [`Self::clear_dynamic_sections`] and before the managed developer
    /// message is synced, so append-mode rule content survives the
    /// per-iteration wipe "for the remainder of the session" — while still
    /// vanishing the instant its
    /// [`SessionEvent::RuleInjection`](crate::session::events::SessionEvent::RuleInjection)
    /// event is compacted or suppressed out of the view (at which point the
    /// rule re-fires on its next trigger). No-op when no rules engine is
    /// installed.
    pub fn materialize_system_context_rules(&mut self, store: &crate::session::store::EventStore) {
        if self.rules.is_none() {
            return;
        }
        let fallback = ContextEdits::new();
        let edits = self.context_edits.as_ref().unwrap_or(&fallback);
        let sections: Vec<String> = store.with_events(|events| {
            let mut sections = Vec::new();
            crate::r#loop::context::for_each_visible_event(events, edits, |event, _tag| {
                if let crate::session::events::SessionEvent::RuleInjection {
                    delivery: crate::rules::types::DeliveryMode::SystemContextAppend,
                    content,
                    ..
                } = event
                {
                    sections.push(content.clone());
                }
            });
            sections
        });
        for section in sections {
            self.append_system_section(section);
        }
    }

    /// Rebuild the rules engine's presence set from the current prompt view.
    ///
    /// Invoked immediately before a tool batch's rule evaluation so
    /// `process_event` suppresses rules already present in context and
    /// re-injects only those whose events have been compacted or suppressed
    /// out of the view (N-007 R7). No-op when no rules engine is installed.
    pub fn rebuild_rule_presence(&mut self, store: &crate::session::store::EventStore) {
        if self.rules.is_none() {
            return;
        }
        let fallback = ContextEdits::new();
        let edits = self.context_edits.as_ref().unwrap_or(&fallback);
        let tags = store.with_events(|events| {
            let mut tags = Vec::new();
            crate::r#loop::context::for_each_visible_event(events, edits, |_event, tag| {
                tags.push(tag);
            });
            tags
        });
        if let Some(engine) = self.rules.as_mut() {
            engine.presence_mut().rebuild(&tags);
        }
    }

    /// Register nested `NORN.md` synthetic rules for a batch of touched
    /// paths before the rules engine evaluates them (NX-004 / NX-005).
    ///
    /// The [`NestedScanner`](crate::context::scanner::NestedScanner) is
    /// lazily constructed from [`Self::working_dir`] on first use, so no
    /// assembly site needs to thread the project root. No-op when no rules
    /// engine is installed or no paths were touched.
    pub fn scan_nested_norn(&mut self, paths: &[String]) {
        if self.rules.is_none() || paths.is_empty() {
            return;
        }
        if self.nested_scanner.is_none() {
            let cwd = self.working_dir.get();
            self.nested_scanner = Some(crate::context::scanner::NestedScanner::new(&cwd));
        }
        if let (Some(scanner), Some(engine)) = (self.nested_scanner.as_mut(), self.rules.as_mut()) {
            for path in paths {
                scanner.scan_on_path_change(path, engine);
            }
        }
    }

    /// Replace the current reasoning effort with `new_effort`, returning
    /// the prior value so the caller can hand it back to
    /// [`Self::restore_reasoning_effort`] after the activation turn.
    ///
    /// Callers that want to preserve the existing effort (for example
    /// because the activating skill has no `effort` field) must simply
    /// skip the override — calling this method always replaces the
    /// stored value.
    pub fn override_reasoning_effort(
        &mut self,
        new_effort: ReasoningEffort,
    ) -> Option<ReasoningEffort> {
        self.reasoning_effort.replace(new_effort)
    }

    /// Restore the reasoning effort to a previously captured value, as
    /// returned by [`Self::override_reasoning_effort`]. Pass `None` to
    /// clear the field (matching the "no effort hint" baseline).
    pub fn restore_reasoning_effort(&mut self, prior: Option<ReasoningEffort>) {
        self.reasoning_effort = prior;
    }

    /// Append the dynamic `# Environment` section when an
    /// [`EnvironmentConfig`] is installed. Gathers current time, working
    /// directory, git branch, and session metadata via Rust APIs (no shell
    /// commands). Called from the runner's iteration top after
    /// [`Self::clear_dynamic_sections`].
    pub fn inject_environment_section(&mut self) {
        if let Some(config) = &self.environment {
            let working_dir = self.working_dir.get();
            let section = format_environment_section(config, &working_dir);
            self.append_system_section(section);
        }
    }

    /// Append the dynamic `# Collaboration Mode` section based on
    /// the current [`CollaborationMode`]. Called from the runner's
    /// iteration top after [`Self::clear_dynamic_sections`].
    pub fn inject_collaboration_mode(&mut self) {
        let section = self.collaboration_mode.format_section();
        self.append_system_section(section);
    }

    /// Evaluate every registered [`PromptCommand`] and append a dynamic
    /// system section per success. Failures (non-zero exit, spawn error,
    /// timeout) are logged via `tracing::warn!` and skipped — the loop
    /// continues without that section.
    ///
    /// Cache misses run **concurrently**, each under `timeout` (`None`
    /// defers to [`DEFAULT_PROMPT_COMMAND_TIMEOUT`], the documented
    /// default; the runner passes
    /// [`AgentLoopConfig::prompt_command_timeout`](crate::agent_loop::config::AgentLoopConfig::prompt_command_timeout)),
    /// so an iteration's prompt-command wall-clock cost is the slowest
    /// command, not the sum. Sections append in registration order
    /// regardless of completion order.
    ///
    /// Callers must invoke this method at the start of every iteration
    /// after [`Self::clear_dynamic_sections`] so the dynamic sections live
    /// for exactly the next provider call.
    pub async fn evaluate_prompt_commands(&mut self, timeout: Option<Duration>) {
        if self.prompt_commands.is_empty() {
            return;
        }
        let timeout = timeout.unwrap_or(DEFAULT_PROMPT_COMMAND_TIMEOUT);
        let commands = self.prompt_commands.clone();
        let now = Instant::now();
        // Resolve cache hits up front; only misses spend a subprocess.
        let cached: Vec<Option<String>> = commands
            .iter()
            .map(|cmd| {
                self.prompt_command_cache
                    .get(&cmd.name)
                    .filter(|entry| entry.expires_at.is_some_and(|deadline| deadline > now))
                    .map(|entry| entry.value.clone())
            })
            .collect();

        let working_dir = self.working_dir.get();
        let misses: Vec<_> = commands
            .iter()
            .zip(&cached)
            .filter(|(_, cached_value)| cached_value.is_none())
            .map(|(cmd, _)| run_prompt_command(&cmd.command, &working_dir, timeout))
            .collect();
        let mut miss_results = futures_util::future::join_all(misses).await.into_iter();

        for (cmd, cached_value) in commands.iter().zip(cached) {
            let outcome = match cached_value {
                Some(value) => Ok(value),
                None => match miss_results.next() {
                    Some(result) => result,
                    // Structurally unreachable: one future was created per
                    // cache miss, in the same order this loop consumes.
                    None => Err("concurrent evaluation produced no result".to_owned()),
                },
            };
            match outcome {
                Ok(stdout) => {
                    if let Some(ttl) = cmd.cache_ttl {
                        self.prompt_command_cache.insert(
                            cmd.name.clone(),
                            PromptCommandCacheEntry {
                                value: stdout.clone(),
                                expires_at: Some(now + ttl),
                            },
                        );
                    } else {
                        // No TTL means caching is disabled; drop any stale entry
                        // so we never accidentally hit it later.
                        self.prompt_command_cache.remove(&cmd.name);
                    }
                    self.append_system_section(format_section(&cmd.name, &stdout));
                }
                Err(err) => {
                    tracing::warn!(
                        command = %cmd.name,
                        error = %err,
                        "prompt command failed; skipping section",
                    );
                }
            }
        }
    }
}

fn format_section(name: &str, body: &str) -> String {
    format!("# {name}\n{body}")
}

async fn run_prompt_command(
    command: &str,
    working_dir: &std::path::Path,
    timeout: Duration,
) -> Result<String, String> {
    let result = tokio::time::timeout(
        timeout,
        Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(working_dir)
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            Ok(stdout
                .trim_end_matches('\n')
                .trim_end_matches('\r')
                .to_owned())
        }
        Ok(Ok(output)) => {
            let exit = output
                .status
                .code()
                .map_or_else(|| "signal".to_owned(), |c| c.to_string());
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            Err(format!("prompt command exited {exit}: {stderr}"))
        }
        Ok(Err(e)) => Err(format!("failed to spawn prompt command: {e}")),
        Err(_) => Err(format!("prompt command timed out after {timeout:?}")),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn,
    clippy::needless_pass_by_value,
    clippy::uninlined_format_args
)]
mod tests {
    use super::*;

    // ---- Rule context lifecycle (N-007 R7 / N-017 R3 / NX-004) ----------

    use crate::rules::engine::RuleEngine;
    use crate::rules::types::{
        DeliveryMode, PathOperation, Rule, RuleId, RuntimeEvent, TriggerCondition, TriggerTiming,
    };
    use crate::session::events::{EventBase, SessionEvent};
    use crate::session::store::EventStore;
    use crate::tool::context::SharedWorkingDir;

    fn append_rule_event(
        store: &EventStore,
        rule_id: &str,
        delivery: DeliveryMode,
        content: &str,
    ) -> crate::session::events::EventId {
        store
            .append(SessionEvent::RuleInjection {
                base: EventBase::new(store.last_event_id()),
                rule_id: rule_id.to_owned(),
                delivery,
                timing: TriggerTiming::After,
                content: content.to_owned(),
            })
            .expect("append")
    }

    fn rs_rule(id: &str, body: &str, delivery: DeliveryMode) -> Rule {
        Rule {
            id: RuleId::from(id),
            name: id.to_owned(),
            triggers: vec![TriggerCondition::PathGlob {
                pattern: "**/*.rs".to_owned(),
            }],
            delivery,
            timing: TriggerTiming::After,
            body: body.to_owned(),
            shell_source: None,
        }
    }

    #[test]
    fn materialize_system_context_rules_reappends_across_wipes() {
        let store = EventStore::new();
        append_rule_event(
            &store,
            "sys-rule",
            DeliveryMode::SystemContextAppend,
            "APPEND_BODY",
        );

        let mut ctx = LoopContext::new("base");
        ctx.rules = Some(RuleEngine::new(vec![]));

        // First prompt-construction pass: content re-materialized from the
        // persisted event even though it was never in `system_sections`.
        ctx.materialize_system_context_rules(&store);
        assert!(ctx.system_instruction().contains("APPEND_BODY"));

        // The per-iteration wipe removes it, but the next pass restores it
        // from the durable event — "for the remainder of the session".
        ctx.clear_dynamic_sections();
        assert!(!ctx.system_instruction().contains("APPEND_BODY"));
        ctx.materialize_system_context_rules(&store);
        assert!(ctx.system_instruction().contains("APPEND_BODY"));
    }

    #[test]
    fn materialize_drops_content_once_event_is_compacted_out() {
        let store = EventStore::new();
        let id = append_rule_event(
            &store,
            "sys-rule",
            DeliveryMode::SystemContextAppend,
            "APPEND_BODY",
        );

        let mut ctx = LoopContext::new("base");
        ctx.rules = Some(RuleEngine::new(vec![]));
        let mut edits = ContextEdits::new();
        edits.suppress(id);
        ctx.context_edits = Some(edits);

        ctx.clear_dynamic_sections();
        ctx.materialize_system_context_rules(&store);
        assert!(
            !ctx.system_instruction().contains("APPEND_BODY"),
            "a compacted/suppressed rule event must not re-materialize",
        );
    }

    #[tokio::test]
    async fn presence_rebuild_suppresses_then_re_fires_after_eviction() {
        let store = EventStore::new();
        let mut ctx = LoopContext::new("base");
        ctx.rules = Some(RuleEngine::new(vec![rs_rule(
            "broad",
            "BODY",
            DeliveryMode::ContextInjection,
        )]));
        ctx.context_edits = Some(ContextEdits::new());

        let event = RuntimeEvent::PathChanged {
            path: "src/lib.rs".to_owned(),
            operation: PathOperation::Read,
        };

        // Fires once when nothing is in context.
        ctx.rebuild_rule_presence(&store);
        let first = ctx.rules.as_ref().unwrap().process_event(&event).await;
        assert_eq!(first.len(), 1, "broad rule must fire on first match");

        // Persist its presence marker; rebuild sees it in context → no re-fire.
        let id = append_rule_event(&store, "broad", DeliveryMode::ContextInjection, "BODY");
        ctx.rebuild_rule_presence(&store);
        assert!(
            ctx.rules
                .as_ref()
                .unwrap()
                .process_event(&event)
                .await
                .is_empty(),
            "rule present in context must not re-fire",
        );

        // Compact the marker out of the view → rule re-fires on next trigger.
        if let Some(edits) = ctx.context_edits.as_mut() {
            edits.suppress(id);
        }
        ctx.rebuild_rule_presence(&store);
        let after = ctx.rules.as_ref().unwrap().process_event(&event).await;
        assert_eq!(after.len(), 1, "evicted rule must re-fire");
    }

    #[tokio::test]
    async fn scan_nested_norn_surfaces_nested_context_once() {
        let cwd = tempfile::tempdir().expect("tempdir");
        let api = cwd.path().join("src").join("api");
        std::fs::create_dir_all(&api).expect("mkdir");
        std::fs::write(api.join("NORN.md"), "API_CONVENTIONS").expect("write");
        std::fs::write(api.join("handler.rs"), "// stub").expect("write");

        let mut ctx =
            LoopContext::with_working_dir("base", SharedWorkingDir::new(cwd.path().to_path_buf()));
        ctx.rules = Some(RuleEngine::new(vec![]));

        // Touching a file under src/api registers the nested NORN.md rule
        // lazily (scanner built from working_dir).
        ctx.scan_nested_norn(&["src/api/handler.rs".to_owned()]);
        // Re-touch: must not register a duplicate.
        ctx.scan_nested_norn(&["src/api/other.rs".to_owned()]);

        let injections = ctx
            .rules
            .as_ref()
            .unwrap()
            .process_event(&RuntimeEvent::PathChanged {
                path: "src/api/handler.rs".to_owned(),
                operation: PathOperation::Read,
            })
            .await;
        assert_eq!(injections.len(), 1, "nested NORN.md surfaces exactly once");
        assert_eq!(injections[0].rule_id.as_str(), "norn-md:src/api");
        assert_eq!(injections[0].content, "API_CONVENTIONS");
    }

    #[test]
    fn new_seeds_single_base_section() {
        let ctx = LoopContext::new("base");
        assert_eq!(ctx.system_sections, vec!["base".to_owned()]);
        assert_eq!(ctx.system_instruction(), "base");
    }

    #[test]
    fn base_system_instruction_returns_only_first_section() {
        let mut ctx = LoopContext::new("base");
        ctx.append_system_section("dynamic-one");
        ctx.append_system_section("dynamic-two");
        assert_eq!(ctx.base_system_instruction(), "base");
    }

    #[test]
    fn base_system_instruction_empty_when_default() {
        let ctx = LoopContext::default();
        assert_eq!(ctx.base_system_instruction(), "");
    }

    #[test]
    fn dynamic_context_none_when_only_base() {
        let ctx = LoopContext::new("base");
        assert!(ctx.dynamic_context().is_none());
    }

    #[test]
    fn dynamic_context_joins_sections_past_base() {
        let mut ctx = LoopContext::new("base");
        ctx.append_system_section("dyn-one");
        ctx.append_system_section("dyn-two");
        assert_eq!(ctx.dynamic_context().unwrap(), "dyn-one\n\ndyn-two",);
    }

    #[test]
    fn dynamic_context_none_after_clear() {
        let mut ctx = LoopContext::new("base");
        ctx.append_system_section("extra");
        ctx.clear_dynamic_sections();
        assert!(ctx.dynamic_context().is_none());
    }

    #[test]
    fn append_and_join_with_double_newline() {
        let mut ctx = LoopContext::new("base");
        ctx.append_system_section("dynamic-one");
        ctx.append_system_section("dynamic-two");
        assert_eq!(
            ctx.system_instruction(),
            "base\n\ndynamic-one\n\ndynamic-two",
        );
    }

    #[test]
    fn clear_dynamic_sections_retains_base() {
        let mut ctx = LoopContext::new("base");
        ctx.append_system_section("extra");
        ctx.append_system_section("more");
        ctx.clear_dynamic_sections();
        assert_eq!(ctx.system_sections, vec!["base".to_owned()]);
        assert_eq!(ctx.system_instruction(), "base");
    }

    #[test]
    fn default_has_no_components() {
        let ctx = LoopContext::default();
        assert!(ctx.rules.is_none());
        assert!(ctx.hooks.is_none());
        assert!(ctx.event_schemas.is_none());
        assert!(ctx.iteration_monitor.is_none());
        assert!(ctx.reasoning_effort.is_none());
        assert!(ctx.slash_commands.is_none());
        assert!(ctx.prompt_commands.is_empty());
        assert!(ctx.prompt_command_cache.is_empty());
        assert_eq!(ctx.retry_policy.max_retries, 2);
        assert!(ctx.token_estimator.is_none());
        assert!(ctx.variables.is_none());
        assert!(ctx.context_edits.is_none());
        assert!(ctx.diagnostics.is_none());
        assert!(ctx.action_log.is_none());
        assert!(ctx.context_loader.is_none());
        assert!(ctx.base_prefix.is_empty());
        assert!(ctx.base_suffix.is_empty());
        assert!(ctx.environment.is_none());
        assert_eq!(ctx.collaboration_mode, CollaborationMode::Default);
        assert!(ctx.child_result_rx.is_none());
        assert_eq!(ctx.children_usage.snapshot().input_tokens, 0);
        assert_eq!(ctx.children_usage.snapshot().output_tokens, 0);
        assert!(ctx.system_sections.is_empty());
        assert_eq!(ctx.system_instruction(), "");
    }

    #[test]
    fn rebuild_base_section_writes_prefix_only_when_loader_and_suffix_absent() {
        let mut ctx = LoopContext::new(String::new());
        ctx.base_prefix = "PREFIX".to_owned();
        ctx.rebuild_base_section();
        assert_eq!(ctx.system_sections, vec!["PREFIX".to_owned()]);
    }

    #[test]
    fn rebuild_base_section_joins_prefix_and_suffix_with_double_newline() {
        let mut ctx = LoopContext::new(String::new());
        ctx.base_prefix = "PREFIX".to_owned();
        ctx.base_suffix = "SUFFIX".to_owned();
        ctx.rebuild_base_section();
        assert_eq!(ctx.system_sections, vec!["PREFIX\n\nSUFFIX".to_owned()]);
    }

    #[test]
    fn rebuild_base_section_skips_empty_parts() {
        let mut ctx = LoopContext::new(String::new());
        ctx.base_suffix = "ONLY-SUFFIX".to_owned();
        ctx.rebuild_base_section();
        assert_eq!(ctx.system_sections, vec!["ONLY-SUFFIX".to_owned()]);
    }

    #[test]
    fn rebuild_base_section_yields_empty_when_everything_absent() {
        let mut ctx = LoopContext::new(String::new());
        ctx.rebuild_base_section();
        assert_eq!(ctx.system_sections, vec![String::new()]);
    }

    #[test]
    fn rebuild_base_section_pushes_when_sections_empty() {
        let mut ctx = LoopContext::default();
        assert!(ctx.system_sections.is_empty());
        ctx.base_prefix = "P".to_owned();
        ctx.rebuild_base_section();
        assert_eq!(ctx.system_sections, vec!["P".to_owned()]);
    }

    #[test]
    fn refresh_context_if_stale_returns_false_without_loader() {
        let mut ctx = LoopContext::new("base");
        assert!(ctx.context_loader.is_none());
        assert!(
            !ctx.refresh_context_if_stale(),
            "no loader wired must surface as not stale"
        );
    }

    #[test]
    #[serial_test::serial]
    fn rebuild_base_section_runs_when_norn_md_changes() {
        // Simulate the runner's iteration top: a single
        // `refresh_context_if_stale` + `rebuild_base_section` cycle must
        // pick up a rewritten project NORN.md. Only the project layer
        // is exercised (cwd is a tempdir) so the test does not touch
        // `$NORN_HOME`.
        let cwd = tempfile::tempdir().unwrap();
        let project_path = cwd.path().join("NORN.md");
        std::fs::write(&project_path, "v1").unwrap();

        let mut ctx = LoopContext::new(String::new());
        ctx.base_prefix = "PRE".to_owned();
        ctx.base_suffix = "POST".to_owned();
        ctx.context_loader = Some(crate::context::ContextLoader::load(cwd.path()));
        ctx.rebuild_base_section();
        assert!(
            ctx.system_sections[0].contains("v1"),
            "precondition: v1 content must appear in the base section",
        );

        // Rewrite the project NORN.md and bump its mtime forward to
        // defeat same-second filesystem-clock granularity.
        std::fs::write(&project_path, "v2").unwrap();
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&project_path)
            .unwrap();
        let future = std::time::SystemTime::now() + std::time::Duration::from_mins(1);
        file.set_modified(future).unwrap();

        assert!(
            ctx.refresh_context_if_stale(),
            "rewritten NORN.md must produce a stale signal",
        );
        ctx.rebuild_base_section();
        assert!(
            ctx.system_sections[0].contains("v2"),
            "rebuild after staleness must inject the new content; got: {}",
            ctx.system_sections[0],
        );
        assert!(
            !ctx.system_sections[0].contains("v1"),
            "rebuild after staleness must drop the prior content; got: {}",
            ctx.system_sections[0],
        );
    }

    #[test]
    #[serial_test::serial]
    fn refresh_context_if_stale_delegates_to_loader_when_present() {
        // Construct a loader pointing at an empty cwd and home — staleness
        // check has no files to observe, so the method must report false.
        let tmp = tempfile::tempdir().unwrap();
        let mut ctx = LoopContext::new("base");
        ctx.context_loader = Some(ContextLoader::load(tmp.path()));

        // Two stat()s on absent files, no state change, no observable
        // staleness — false.
        assert!(!ctx.refresh_context_if_stale());
    }

    #[test]
    fn new_has_no_diagnostics() {
        let ctx = LoopContext::new("base");
        assert!(ctx.diagnostics.is_none());
    }

    #[test]
    fn diagnostics_roundtrips() {
        use crate::integration::{DiagnosticCollector, DiagnosticSeverity, NornDiagnostic};

        let mut ctx = LoopContext::new("base");
        let collector = Arc::new(DiagnosticCollector::new());
        ctx.diagnostics = Some(Arc::clone(&collector));

        let diag = NornDiagnostic {
            severity: DiagnosticSeverity::Warning,
            code: "tool-blocked".to_owned(),
            message: "blocked".to_owned(),
            source_tool: Some("write".to_owned()),
            file_path: None,
            suggestion: None,
        };
        ctx.diagnostics.as_ref().unwrap().report(diag);

        let drained = collector.drain();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].code, "tool-blocked");
        assert_eq!(drained[0].severity, DiagnosticSeverity::Warning);
    }

    #[test]
    fn override_reasoning_effort_returns_prior_none_and_sets_some() {
        let mut ctx = LoopContext::new("base");
        assert!(ctx.reasoning_effort.is_none());
        let prior = ctx.override_reasoning_effort(ReasoningEffort::High);
        assert!(prior.is_none());
        assert_eq!(ctx.reasoning_effort, Some(ReasoningEffort::High));
    }

    #[test]
    fn override_reasoning_effort_returns_prior_some_value() {
        let mut ctx = LoopContext::new("base");
        ctx.reasoning_effort = Some(ReasoningEffort::Low);
        let prior = ctx.override_reasoning_effort(ReasoningEffort::XHigh);
        assert_eq!(prior, Some(ReasoningEffort::Low));
        assert_eq!(ctx.reasoning_effort, Some(ReasoningEffort::XHigh));
    }

    #[test]
    fn restore_reasoning_effort_returns_field_to_prior_some() {
        let mut ctx = LoopContext::new("base");
        ctx.reasoning_effort = Some(ReasoningEffort::Medium);
        let prior = ctx.override_reasoning_effort(ReasoningEffort::XHigh);
        ctx.restore_reasoning_effort(prior);
        assert_eq!(ctx.reasoning_effort, Some(ReasoningEffort::Medium));
    }

    #[test]
    fn restore_reasoning_effort_returns_field_to_prior_none() {
        let mut ctx = LoopContext::new("base");
        let prior = ctx.override_reasoning_effort(ReasoningEffort::High);
        ctx.restore_reasoning_effort(prior);
        assert!(ctx.reasoning_effort.is_none());
    }

    #[test]
    fn caller_that_skips_override_leaves_reasoning_effort_untouched() {
        // Models the call-site contract: when a skill carries no effort
        // field, the caller skips override_reasoning_effort entirely and
        // the loop's value stays untouched.
        let mut ctx = LoopContext::new("base");
        ctx.reasoning_effort = Some(ReasoningEffort::Medium);
        let mapped: Option<ReasoningEffort> = None; // stands in for None skill effort
        if let Some(eff) = mapped {
            let _ = ctx.override_reasoning_effort(eff);
        }
        assert_eq!(ctx.reasoning_effort, Some(ReasoningEffort::Medium));
    }

    #[tokio::test]
    async fn evaluate_prompt_commands_appends_stdout_section() {
        let mut ctx = LoopContext::new("base");
        ctx.prompt_commands.push(PromptCommand {
            name: "greet".to_owned(),
            command: "echo hello".to_owned(),
            cache_ttl: None,
        });
        ctx.evaluate_prompt_commands(None).await;
        let combined = ctx.system_instruction();
        assert!(
            combined.contains("hello"),
            "system instruction must contain command stdout: {combined}",
        );
        assert!(
            combined.contains("greet"),
            "system instruction must contain command name heading: {combined}",
        );
    }

    #[tokio::test]
    async fn evaluate_prompt_commands_failure_skips_section() {
        let mut ctx = LoopContext::new("base");
        ctx.prompt_commands.push(PromptCommand {
            name: "fail".to_owned(),
            command: "exit 7".to_owned(),
            cache_ttl: None,
        });
        ctx.evaluate_prompt_commands(None).await;
        let combined = ctx.system_instruction();
        assert_eq!(
            combined, "base",
            "failing prompt command must not append a section",
        );
    }

    #[tokio::test]
    async fn evaluate_prompt_commands_caches_within_ttl() {
        let mut ctx = LoopContext::new("base");
        ctx.prompt_commands.push(PromptCommand {
            name: "stamp".to_owned(),
            command: "date +%N || echo stable".to_owned(),
            cache_ttl: Some(Duration::from_mins(1)),
        });
        ctx.evaluate_prompt_commands(None).await;
        let first = ctx.system_instruction();
        ctx.clear_dynamic_sections();
        ctx.evaluate_prompt_commands(None).await;
        let second = ctx.system_instruction();
        assert_eq!(
            first, second,
            "second evaluation within TTL must reuse cache"
        );
    }

    /// Two 300ms commands must evaluate concurrently: the iteration's
    /// prompt-command cost is the slowest command, not the sum. Serial
    /// evaluation would take at least 600ms.
    #[tokio::test]
    async fn evaluate_prompt_commands_runs_misses_concurrently_in_order() {
        let mut ctx = LoopContext::new("base");
        ctx.prompt_commands.push(PromptCommand {
            name: "first".to_owned(),
            command: "sleep 0.3 && echo one".to_owned(),
            cache_ttl: None,
        });
        ctx.prompt_commands.push(PromptCommand {
            name: "second".to_owned(),
            command: "sleep 0.3 && echo two".to_owned(),
            cache_ttl: None,
        });

        let started = Instant::now();
        ctx.evaluate_prompt_commands(None).await;
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_millis(550),
            "two 300ms commands must overlap, took {elapsed:?}",
        );
        // Sections append in registration order, not completion order.
        assert_eq!(ctx.system_sections.len(), 3);
        assert_eq!(ctx.system_sections[1], "# first\none");
        assert_eq!(ctx.system_sections[2], "# second\ntwo");
    }

    /// A configured `prompt_command_timeout` overrides the documented
    /// 5-second default: a command slower than the budget is cut and its
    /// section skipped, without waiting out the default.
    #[tokio::test]
    async fn evaluate_prompt_commands_honors_configured_timeout() {
        let mut ctx = LoopContext::new("base");
        ctx.prompt_commands.push(PromptCommand {
            name: "slowpoke".to_owned(),
            command: "sleep 2 && echo late".to_owned(),
            cache_ttl: None,
        });

        let started = Instant::now();
        ctx.evaluate_prompt_commands(Some(Duration::from_millis(100)))
            .await;
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_millis(900),
            "the configured budget must cut the command, took {elapsed:?}",
        );
        assert_eq!(
            ctx.system_instruction(),
            "base",
            "a timed-out prompt command must not append a section",
        );
    }

    #[test]
    fn inject_environment_section_appends_when_configured() {
        let mut ctx = LoopContext::new("base");
        ctx.environment = Some(EnvironmentConfig {
            session_id: Some("test-session".to_owned()),
            model: "gpt-5.5".to_owned(),
        });
        ctx.inject_environment_section();
        let combined = ctx.system_instruction();
        assert!(
            combined.contains("# Environment"),
            "environment section must be appended: {combined}",
        );
        assert!(
            combined.contains("Model: gpt-5.5"),
            "environment section must include model: {combined}",
        );
        assert!(
            combined.contains("Session: test-session"),
            "environment section must include session: {combined}",
        );
    }

    #[test]
    fn inject_environment_section_noop_when_not_configured() {
        let mut ctx = LoopContext::new("base");
        assert!(ctx.environment.is_none());
        ctx.inject_environment_section();
        assert_eq!(
            ctx.system_instruction(),
            "base",
            "no environment config means no section appended",
        );
    }

    #[test]
    fn inject_environment_section_refreshes_after_clear() {
        let mut ctx = LoopContext::new("base");
        ctx.environment = Some(EnvironmentConfig {
            session_id: None,
            model: "gpt-5.5".to_owned(),
        });
        ctx.inject_environment_section();
        assert!(ctx.system_instruction().contains("# Environment"));

        ctx.clear_dynamic_sections();
        assert!(
            !ctx.system_instruction().contains("# Environment"),
            "clear must remove environment section",
        );

        ctx.inject_environment_section();
        assert!(
            ctx.system_instruction().contains("# Environment"),
            "re-injection must restore environment section",
        );
    }

    #[test]
    fn inject_collaboration_mode_default_appends_section() {
        let mut ctx = LoopContext::new("base");
        ctx.inject_collaboration_mode();
        let instruction = ctx.system_instruction();
        assert!(
            instruction.contains("# Collaboration Mode"),
            "default mode should inject a section",
        );
        assert!(
            instruction.contains("prefer making reasonable assumptions"),
            "default mode should contain default guidance",
        );
    }

    #[test]
    fn inject_collaboration_mode_plan_contains_phases() {
        let mut ctx = LoopContext::new("base");
        ctx.collaboration_mode = CollaborationMode::Plan;
        ctx.inject_collaboration_mode();
        let instruction = ctx.system_instruction();
        assert!(instruction.contains("plan mode"));
        assert!(instruction.contains("Ground in the environment"));
        assert!(instruction.contains("Not allowed"));
    }

    #[test]
    fn inject_collaboration_mode_autonomous_contains_persist() {
        let mut ctx = LoopContext::new("base");
        ctx.collaboration_mode = CollaborationMode::Autonomous;
        ctx.inject_collaboration_mode();
        let instruction = ctx.system_instruction();
        assert!(instruction.contains("autonomous execution mode"));
        assert!(instruction.contains("Persist until the task is fully handled"));
    }

    #[test]
    fn collaboration_mode_changes_mid_session() {
        let mut ctx = LoopContext::new("base");
        ctx.inject_collaboration_mode();
        assert!(ctx.system_instruction().contains("reasonable assumptions"));

        ctx.clear_dynamic_sections();
        ctx.collaboration_mode = CollaborationMode::Plan;
        ctx.inject_collaboration_mode();
        assert!(ctx.system_instruction().contains("plan mode"));
        assert!(!ctx.system_instruction().contains("reasonable assumptions"));
    }
}
