//! Fluent configuration setters for
//! [`AgentBuilder`](crate::agent::builder::AgentBuilder).
//!
//! Split from `agent/builder.rs` to keep each file within the
//! production-size limit: `builder.rs` owns construction, validation,
//! and assembly (`new` / `build` / `run`); this module owns the fluent
//! surface that records what to assemble.

use std::any::Any;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::builder::AgentBuilder;
use crate::agent::child_policy::ChildPolicy;
use crate::agent::registry::AgentRegistry;
use crate::agent::session_spec::{SessionRequest, SessionSpec};
use crate::agent_loop::config::{AgentLoopConfig, ConversationStateMode};
use crate::agent_loop::event_schemas::EventSchemaSet;
use crate::agent_loop::inbound::{InboundSender, inbound_channel};
use crate::agent_loop::retry::RetryPolicy;
use crate::integration::DiagnosticCollector;
use crate::integration::hooks::HookRegistry;
use crate::integration::variables::VariableStore;
use crate::profile::{Capability, Profile};
use crate::provider::request::{ReasoningEffort, ServiceTier};
use crate::rules::engine::RuleEngine;
use crate::session::SessionManager;
use crate::session::store::{DurabilityPolicy, EventStore};
use crate::system_prompt::builder::ExecutionMode;
use crate::tool::lifecycle::RuntimePostValidateCheck;
use crate::tool::traits::Tool;
use crate::tools::diagnostics::DiagnosticInfra;
use crate::tools::lsp::{LspBackend, LspWorkspace};

impl AgentBuilder {
    /// Load the same settings, NORN.md context, skill catalog, discovered
    /// rules, hook registry, retry policy, and agent-loop config used by the
    /// CLI before applying explicit builder overrides.
    #[must_use]
    pub fn load_runtime_base(mut self) -> Self {
        self.load_runtime_base = true;
        self
    }

    /// Select the task-store group slug used when [`Self::load_runtime_base`]
    /// installs the disk-backed task store.
    #[must_use]
    pub fn task_group_slug(mut self, slug: impl Into<String>) -> Self {
        self.task_group_slug = Some(slug.into());
        self
    }

    /// Use an already-loaded profile (capabilities, model, instructions).
    #[must_use]
    pub fn profile(mut self, profile: Profile) -> Self {
        self.profile = Some(profile);
        self
    }

    /// Resolve a profile by bare name at build time, searching
    /// `.norn/profiles` then `.meridian/profiles` then `~/.norn/profiles`
    /// relative to the working directory. Ignored when [`Self::profile`] is
    /// also set.
    #[must_use]
    pub fn profile_name(mut self, name: impl Into<String>) -> Self {
        self.profile_name = Some(name.into());
        self
    }

    /// Override the model the profile selects.
    #[must_use]
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Override the profile's system instructions.
    #[must_use]
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Append additional instructions after the resolved profile instructions
    /// or caller-supplied [`Self::system_prompt`] override.
    #[must_use]
    pub fn append_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.append_system_prompt = Some(prompt.into());
        self
    }

    /// Override the profile's reasoning-effort hint.
    #[must_use]
    pub fn reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    /// Override the profile's service-tier hint.
    #[must_use]
    pub fn service_tier(mut self, tier: ServiceTier) -> Self {
        self.service_tier = Some(tier);
        self
    }

    /// Add capability bundles to the resolved profile before tool gating and
    /// prompt construction.
    #[must_use]
    pub fn capabilities(mut self, capabilities: Vec<Capability>) -> Self {
        self.capabilities.extend(capabilities);
        self
    }

    /// Set the agent's working directory. Defaults to the process current
    /// directory when unset. All filesystem tools resolve relative paths
    /// against this, and `bash` `cd` directives update it.
    #[must_use]
    pub fn working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(dir.into());
        self
    }

    /// Confine the file tools (`read` / `write` / `edit` / `apply_patch`)
    /// to `root`: any path that resolves outside it after symlink-aware
    /// canonicalization is refused, including `..` traversal, absolute
    /// paths, and symlink escapes. `bash` checks its model-supplied
    /// `working_dir` argument against the root but cannot confine what the
    /// command itself does — a known, documented limitation.
    ///
    /// The root must exist and be a directory when [`Self::build`] runs,
    /// otherwise building fails with a configuration error. Unset (the
    /// default) leaves path resolution unconfined for embedders that
    /// operate across arbitrary directories.
    #[must_use]
    pub fn workspace_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.workspace_root = Some(root.into());
        self
    }

    /// Override the grace period the `bash` tool grants its output drains
    /// after the shell exits — the bound on how long a backgrounded child
    /// (`server &`) can hold the output pipes before the tool returns the
    /// buffered output annotated with `streams_still_open`.
    ///
    /// Defaults to 2 seconds (the owner-approved default) when unset.
    /// Applies to the standard `bash` tool; building fails when this is
    /// set but `bash` is excluded from the final tool set.
    #[must_use]
    pub fn bash_drain_grace(mut self, grace: Duration) -> Self {
        self.bash_drain_grace = Some(grace);
        self
    }

    /// Restrict the default/profile tool set to the named tools.
    #[must_use]
    pub fn allowed_tools(mut self, names: &[&str]) -> Self {
        self.allowed_tools
            .replace(names.iter().map(|s| (*s).to_string()).collect());
        self
    }

    /// Exclude specific tools from the default all-tools set (e.g. mutation
    /// tools for a read-only scout step). Names match the Norn tool registry
    /// names (`bash`, `write`, `edit`, …).
    #[must_use]
    pub fn without_tools(mut self, names: &[&str]) -> Self {
        self.without_tools
            .extend(names.iter().map(|s| (*s).to_string()));
        self
    }

    /// Add a custom tool alongside the standard set.
    #[must_use]
    pub fn tool(mut self, tool: Box<dyn Tool + Send + Sync>) -> Self {
        self.extra_tools.push(tool);
        self
    }

    /// Wire a live LSP backend for the `lsp` tool. Without one, `lsp` is
    /// registered but every call returns a "configure a backend" error.
    #[must_use]
    pub fn lsp_backend(mut self, backend: Arc<dyn LspBackend>) -> Self {
        self.lsp_backend = Some(backend);
        self
    }

    /// Wire a live LSP workspace for diagnostics post-checks. Without one,
    /// diagnostics still run through server / inline adapters but skip the LSP
    /// fast path.
    #[must_use]
    pub fn lsp_workspace(mut self, workspace: Arc<LspWorkspace>) -> Self {
        self.lsp_workspace = Some(workspace);
        self
    }

    /// Enforce a structured-output JSON schema on the final response.
    ///
    /// Stored on the agent-loop config
    /// ([`AgentLoopConfig::output_schema`](crate::agent_loop::config::AgentLoopConfig::output_schema)),
    /// so it serializes with the rest of the config and is introspectable
    /// post-build via
    /// [`ResolvedAgentInfo::output_schema`](crate::agent::ResolvedAgentInfo::output_schema).
    #[must_use]
    pub fn output_schema(mut self, schema: Value) -> Self {
        self.agent_config.output_schema = Some(schema);
        self
    }

    /// Interactive vs headless execution (shapes the system prompt). Defaults
    /// to [`ExecutionMode::Headless`] for library use.
    #[must_use]
    pub fn execution_mode(mut self, mode: ExecutionMode) -> Self {
        self.execution_mode = mode;
        self
    }

    /// Replace the whole agent-loop config (schema budget, max iterations,
    /// step timeout, compaction, cache key).
    ///
    /// Supplying a complete config marks every non-`Option` field
    /// explicit: when [`Self::load_runtime_base`] is also set, these fields
    /// win over the settings-derived base even where they equal the library
    /// default (an explicit `schema_budget = 3` is honoured, not reverted
    /// to a settings value). `Option` fields still overlay only when
    /// `Some`.
    #[must_use]
    pub fn agent_config(mut self, config: AgentLoopConfig) -> Self {
        self.agent_config = config;
        self.agent_config_present = crate::agent::assembly::AgentConfigPresence::all();
        self
    }

    /// Cap total provider round-trips per step.
    #[must_use]
    pub fn max_iterations(mut self, max: u32) -> Self {
        self.agent_config.max_iterations = Some(max);
        self
    }

    /// Set an outer wall-clock cap on the whole step.
    #[must_use]
    pub fn step_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.agent_config.step_timeout = Some(timeout);
        self
    }

    /// Set the client-side context-window budget, in tokens, that drives the
    /// token estimator and the auto-compaction trigger.
    ///
    /// Granular counterpart to [`Self::agent_config`]: it overrides only
    /// `context_window_limit`, leaving every other field of the
    /// settings-derived runtime base intact (unlike `agent_config`, which
    /// marks the whole config explicit). Because the field carries its own
    /// presence (`Some` = explicitly set), this value wins over the runtime
    /// base per the explicit-config-wins merge, and auto-compaction arming
    /// honours it — the catalog window is filled only when this is left
    /// `None`.
    #[must_use]
    pub fn context_window_limit(mut self, tokens: u64) -> Self {
        self.agent_config.context_window_limit = Some(tokens);
        self
    }

    /// Select how the provider carries conversation state between calls.
    #[must_use]
    pub fn conversation_state(mut self, mode: ConversationStateMode) -> Self {
        self.agent_config.conversation_state = mode;
        self.agent_config_present.conversation_state = true;
        self
    }

    /// Set the provider-side compaction threshold in rendered tokens.
    #[must_use]
    pub fn server_compaction_threshold_tokens(mut self, tokens: u64) -> Self {
        self.agent_config.server_compaction_threshold_tokens = Some(tokens);
        self
    }

    /// Configure provider retry. Defaults to [`RetryPolicy::default`]
    /// (2 retries, 1s initial backoff, 2x multiplier) when unset.
    #[must_use]
    pub fn retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = Some(policy);
        self
    }

    /// Resume a session from a prior run's in-memory [`EventStore`]. A
    /// fresh store is created when unset. For disk-persisted sessions,
    /// prefer [`Self::open_session`] — the two are mutually exclusive and
    /// setting both fails the build.
    #[must_use]
    pub fn session(mut self, store: EventStore) -> Self {
        self.session = Some(store);
        self
    }

    /// Open (create / resume / fork / open-or-resume, per `spec`) a
    /// disk-persisted session through `manager` at build time, and wire
    /// the agent to it end to end:
    ///
    /// - the returned sink-equipped store becomes the agent's session
    ///   store (every event persists per `durability`),
    /// - the session's index-entry id becomes the loop's prompt
    ///   `cache_key` and the system prompt environment's `session_id`,
    ///   and is surfaced on
    ///   [`ResolvedAgentInfo::session_id`](crate::agent::ResolvedAgentInfo::session_id),
    /// - the index entry and replay summary are surfaced via
    ///   [`Agent::session_entry`](crate::agent::Agent::session_entry) /
    ///   [`Agent::session_replay`](crate::agent::Agent::session_replay); tolerant-reader
    ///   skips are additionally logged at warn level.
    ///
    /// The new index entry records the *resolved* model and working
    /// directory (after profile resolution and overrides), so the
    /// persisted record always matches what the agent actually ran with.
    ///
    /// Mutually exclusive with [`Self::session`]; also conflicts with an
    /// explicitly set
    /// [`AgentLoopConfig::cache_key`](crate::agent_loop::config::AgentLoopConfig::cache_key)
    /// (the session id *is* the cache key on this path). Either conflict
    /// fails the build with a typed configuration error.
    #[must_use]
    pub fn open_session(
        mut self,
        manager: &SessionManager,
        spec: SessionSpec,
        durability: DurabilityPolicy,
    ) -> Self {
        self.session_request = Some(SessionRequest {
            manager: manager.clone(),
            spec,
            durability,
        });
        self
    }

    /// Create the agent's event broadcast channel with this capacity.
    ///
    /// The builder constructs the channel and the root
    /// [`AgentEventSender`](crate::provider::agent_event::AgentEventSender)
    /// (tagged with the agent's id and the `root`
    /// role) itself, publishes the raw channel on the tool context as
    /// [`SharedAgentEventChannel`](crate::provider::agent_event::SharedAgentEventChannel)
    /// so fork/spawn children stream through
    /// the same channel, and exposes subscriptions via
    /// [`AgentHandle::subscribe`](crate::agent::AgentHandle::subscribe).
    ///
    /// The capacity is explicit rather than defaulted: the right buffer
    /// depends on how fast consumers drain relative to the model's output
    /// rate. Zero fails the build. Without this call the agent emits no
    /// events and [`AgentHandle::subscribe`](crate::agent::AgentHandle::subscribe)
    /// returns `None`.
    #[must_use]
    pub fn event_channel_capacity(mut self, capacity: usize) -> Self {
        self.event_channel_capacity = Some(capacity);
        self
    }

    /// Thread a cancellation token into the loop for cooperative abort.
    ///
    /// Use this to *link* cancellation with an embedder-owned token tree
    /// (e.g. a durable-workflow engine's activity token — pass its
    /// `child_token()`). When unset, the builder creates a fresh token;
    /// either way [`AgentHandle::cancel`](crate::agent::AgentHandle::cancel)
    /// and [`AgentHandle::cancellation_token`](crate::agent::AgentHandle::cancellation_token)
    /// operate on the token the loop honors.
    #[must_use]
    pub fn cancel_token(mut self, token: CancellationToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// Create the agent's inbound steering channel with this capacity.
    ///
    /// The builder constructs the channel pair itself: the receiver is
    /// wired into the root agent step so mid-session messages (e.g. DMs
    /// while the agent is mid-turn) are drained at every tool boundary,
    /// and the sender is available immediately via
    /// [`Self::inbound_sender`] (for infrastructure built before the
    /// agent, e.g. notification injectors) and after build via
    /// [`AgentHandle::inbound_sender`](crate::agent::AgentHandle::inbound_sender).
    ///
    /// The capacity is explicit rather than defaulted; producers awaiting
    /// `send` block when the buffer is full. Zero fails the build.
    /// Without this call the root step has no mid-session injection path
    /// — only the initial [`Agent::run`](crate::agent::Agent::run) prompt
    /// enters the conversation.
    #[must_use]
    pub fn inbound_capacity(mut self, capacity: usize) -> Self {
        self.inbound_capacity = Some(capacity);
        if capacity > 0 {
            let (tx, rx) = inbound_channel(capacity);
            self.inbound = Some(rx);
            self.inbound_tx = Some(tx);
        }
        self
    }

    /// The sender half of the inbound steering channel created by
    /// [`Self::inbound_capacity`]; `None` until that is called. Cheap to
    /// clone — grab it mid-chain for infrastructure that must exist
    /// before [`Self::build`].
    #[must_use]
    pub fn inbound_sender(&self) -> Option<InboundSender> {
        self.inbound_tx.clone()
    }

    /// Set the agent's id (sender identity for messaging, parent id for
    /// spawned children). A fresh id is generated when unset.
    #[must_use]
    pub fn agent_id(mut self, id: Uuid) -> Self {
        self.agent_id = Some(id);
        self
    }

    /// Wire a programmatic hook registry.
    #[must_use]
    pub fn hooks(mut self, hooks: Arc<HookRegistry>) -> Self {
        self.hooks = Some(hooks);
        self
    }

    /// Wire a rules engine (context injection / guardrails).
    #[must_use]
    pub fn rules(mut self, rules: RuleEngine) -> Self {
        self.rules = Some(rules);
        self
    }

    /// Wire a diagnostic collector. Published on the tool context and the
    /// loop context so post-validation checks can record diagnostics.
    #[must_use]
    pub fn diagnostics(mut self, diagnostics: Arc<DiagnosticCollector>) -> Self {
        self.diagnostics = Some(diagnostics);
        self
    }

    /// Wire diagnostic infrastructure and install the diagnostics post-check.
    #[must_use]
    pub fn diagnostic_infra(mut self, infra: Arc<DiagnosticInfra>) -> Self {
        self.diagnostic_infra = Some(infra);
        self
    }

    /// Add a runtime post-validation check to the agent's tool context.
    ///
    /// Checks added here run after the diagnostics post-check installed by
    /// [`Self::diagnostic_infra`], when diagnostic infrastructure is present.
    #[must_use]
    pub fn post_check(mut self, check: Box<dyn RuntimePostValidateCheck>) -> Self {
        self.additional_post_checks.push(check);
        self
    }

    /// Wire the shared agent registry so `fork` / `spawn_agent` /
    /// `signal_agent` / `close_agent` resolve their runtime instead of
    /// erroring with a typed `MissingExtension` error naming
    /// `AgentToolInfra`.
    ///
    /// Wiring this makes the coordination envelope mandatory:
    /// [`Self::child_policy`] and [`Self::child_result_capacity`] must
    /// both be set, otherwise [`Self::build`] fails with a typed
    /// configuration error — Norn never assumes a default child policy
    /// or channel capacity.
    #[must_use]
    pub fn agent_registry(mut self, registry: Arc<RwLock<AgentRegistry>>) -> Self {
        self.agent_registry = Some(registry);
        self
    }

    /// Set the root coordination envelope's [`ChildPolicy`] — this
    /// agent's own granted policy (messaging scope, delegation budget,
    /// child inbound-channel capacity), from which every child's grant
    /// is derived at spawn/fork time.
    ///
    /// **Required** whenever [`Self::agent_registry`] is wired; building
    /// without it is a typed configuration error. Conversely, setting it
    /// without [`Self::agent_registry`] also fails the build — it would
    /// otherwise be silently ignored. There is no library default.
    ///
    /// Recommended starting envelope (a documented proposal matching
    /// today's production-proven behaviour, never assumed):
    ///
    /// ```
    /// use norn::agent::child_policy::{ChildPolicy, DelegationBudget, MessagingScope};
    ///
    /// let policy = ChildPolicy {
    ///     messaging: MessagingScope::SiblingsAndParent,
    ///     delegation: DelegationBudget {
    ///         remaining_depth: 1,
    ///         max_concurrent_children: 32,
    ///     },
    ///     inbound_capacity: 32,
    ///     loop_config: None,
    /// };
    /// # let _ = policy;
    /// ```
    ///
    /// `inbound_capacity` must be non-zero; zero fails the build. The
    /// policy is published on the shared tool context as part of the
    /// [`CoordinationEnvelope`](crate::agent::child_policy::CoordinationEnvelope).
    /// It is this agent's **own** granted policy: its spawn/fork
    /// reservations are checked against `delegation` (a `remaining_depth`
    /// of 0 disables delegation entirely), and every child it creates is
    /// granted this policy with the depth decremented one level — or a
    /// per-spawn narrowing of it — sizing the child's inbound channel and
    /// fixing its `signal_agent` scope (W3.2/W3.4).
    #[must_use]
    pub fn child_policy(mut self, policy: ChildPolicy) -> Self {
        self.child_policy = Some(policy);
        self
    }

    /// Set the bounded capacity of this agent's child-result channel —
    /// the channel through which spawned and forked children deliver
    /// their results to this agent's loop.
    ///
    /// **Required** whenever [`Self::agent_registry`] is wired; building
    /// without it is a typed configuration error, as is setting it
    /// without [`Self::agent_registry`] (it would be silently ignored).
    /// Zero fails the build. There is no library default; the documented
    /// proposal is 256 — generous enough that child completion never
    /// blocks under normal operation, while a full channel still signals
    /// runaway spawning.
    #[must_use]
    pub fn child_result_capacity(mut self, capacity: usize) -> Self {
        self.child_result_capacity = Some(capacity);
        self
    }

    /// Install a set of per-event-type output schemas the loop enforces
    /// on model-emitted structured events. There is no library default —
    /// unset leaves the loop's event-schema enforcement dark.
    #[must_use]
    pub fn event_schemas(mut self, schemas: EventSchemaSet) -> Self {
        self.event_schemas = Some(schemas);
        self
    }

    /// Provide the variable store the loop expands `{{…}}` templates
    /// against, replacing the built-in store
    /// [`build`](Self::build) would otherwise mint.
    ///
    /// When set, the store's `session_id` must agree with the session id
    /// [`build`](Self::build) resolves (the minted id, or the persisted
    /// id from [`Self::open_session`]); a mismatch fails the build with a
    /// typed configuration error rather than silently running the loop and
    /// the persisted session under two different ids. There is no library
    /// default beyond the built-in store `build` mints when this is unset.
    #[must_use]
    pub fn variables(mut self, variables: Arc<VariableStore>) -> Self {
        self.variables = Some(variables);
        self
    }

    /// Add raw `name`/`value` static variable pairs to the store
    /// [`build`](Self::build) mints, instead of handing in a pre-built
    /// [`VariableStore`] via [`Self::variables`].
    ///
    /// This is the identity-safe path for callers (notably `norn-cli`'s
    /// `--variables KEY=VALUE`) that have no session id of their own: the
    /// pairs are applied to the store `build` creates with the **resolved**
    /// session id (the minted id, or the persisted id from
    /// [`Self::open_session`]), so there is no independently-minted store id
    /// to disagree with an `open_session`-pinned session and abort the
    /// build. Additive across calls; may be combined with
    /// [`Self::variables`] (the pairs then land on the supplied store).
    #[must_use]
    pub fn variable_pairs(mut self, pairs: Vec<(String, String)>) -> Self {
        self.variable_pairs.extend(pairs);
        self
    }

    /// Deny the named tools after profile/allow-list gating, with
    /// deny-wins semantics: a disallowed name stays unavailable even when
    /// the allow-list ([`Self::allowed_tools`]) names it. Matched by exact
    /// tool-registry name, mirroring
    /// [`ToolRegistry::set_disallowed`](crate::tool::registry::ToolRegistry::set_disallowed).
    /// There is no library default — unset denies nothing.
    #[must_use]
    pub fn disallowed_tools(mut self, names: &[&str]) -> Self {
        self.disallowed_tools = names.iter().map(|s| (*s).to_string()).collect();
        self
    }

    /// Control whether the agent-coordination runtime installs the
    /// terminal-reclamation marker.
    ///
    /// Only meaningful when [`Self::agent_registry`] is wired. Defaults to
    /// `true` — every headless / embedded runtime reclaims a finished
    /// child's terminal registry entry once its result is delivered. A
    /// driver that owns reclamation itself through a status panel (the
    /// TUI) passes `false`, otherwise the marker would race its hold
    /// window into nonexistence.
    #[must_use]
    pub fn terminal_reclamation(mut self, enabled: bool) -> Self {
        self.terminal_reclamation = enabled;
        self
    }

    /// Reserve and confirm the agent-registry `path` entry (with `role`)
    /// for this agent at build time, using the coordination envelope's
    /// child policy as the root's granted budget, and adopt the reserved
    /// id as the agent's id.
    ///
    /// Opt-in and effective **only** alongside [`Self::agent_registry`]:
    /// embedders that wire no coordination (no `spawn`/`fork`) need no
    /// root entry, so root registration is never mandatory. Setting this
    /// without [`Self::agent_registry`] fails the build — it would
    /// otherwise be silently ignored. The reserved id supersedes any
    /// [`Self::agent_id`], since the registered root entry and the running
    /// agent must share one id.
    #[must_use]
    pub fn register_root(mut self, path: String, role: String) -> Self {
        self.register_root = Some((path, role));
        self
    }

    /// Publish a typed extension on the agent's shared
    /// [`ToolContext`](crate::tool::context::ToolContext) at build time,
    /// retrievable inside tools via
    /// [`ToolContext::get_extension`](crate::tool::context::ToolContext::get_extension).
    ///
    /// Extensions are installed after the standard and extra tools are
    /// registered and after profile gating, so embedding consumers can attach
    /// host-supplied infrastructure (identity, service handles, registries)
    /// that individual tools read at execution time. Inserting two values of
    /// the same type keeps only the last, matching
    /// [`ToolContext::insert_extension`](crate::tool::context::ToolContext::insert_extension)
    /// semantics.
    #[must_use]
    pub fn extension<T>(mut self, value: Arc<T>) -> Self
    where
        T: Any + Send + Sync,
    {
        self.extensions
            .push(Box::new(move |ctx| ctx.insert_extension(value)));
        self
    }
}
