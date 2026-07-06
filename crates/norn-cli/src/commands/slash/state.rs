//! Shared mutable state captured by the CLI slash-command handlers.
//!
//! Each handler closure has the signature
//! `Fn(&str) -> Result<Vec<Message>, NornError>` so it cannot accept
//! per-call context. Runtime state (the active model name, the active
//! output schema, the session name, cumulative usage, etc.) therefore
//! lives behind [`Arc`] inside [`SlashState`]; closures clone the
//! relevant [`Arc`] fields and the orchestrator reads them back through
//! the same cells after dispatch.
//!
//! Action flags ([`compact_requested`](SlashState::compact_requested),
//! [`clear_requested`](SlashState::clear_requested),
//! [`exit_requested`](SlashState::exit_requested)) carry effects that
//! cannot be applied inside the closure itself — compaction needs
//! `&mut LoopContext::context_edits`, clearing replaces the event store,
//! and exit unwinds the REPL loop. The closure flips the bit and the
//! orchestrator picks it up after `preprocess_input`.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use norn::provider::request::{ReasoningEffort, ServiceTier};
use norn::provider::usage::Usage;
use norn::session::store::EventStore;
use parking_lot::Mutex;
use serde_json::Value;

/// Shared mutable runtime state captured by the CLI slash-command
/// handlers.
///
/// Constructed once when the runtime bundle is assembled and cloned
/// (cheaply — every field is either `Arc` or a small owned value) into
/// every handler closure that needs to read or mutate the corresponding
/// cell. Orchestrators (print mode, REPL) hold the same `SlashState`
/// so they can inspect action flags and the latest values after a
/// slash command returns.
#[derive(Clone)]
pub struct SlashState {
    /// Active model identifier. Read by the orchestrator before each
    /// `run_agent_step` call so `/model gpt-x` takes effect immediately.
    pub model: Arc<Mutex<String>>,

    /// Active service tier. Read by the orchestrator before each
    /// `run_agent_step` call so `/service-tier fast` takes effect
    /// immediately.
    pub service_tier: Arc<Mutex<Option<ServiceTier>>>,

    /// Active reasoning effort. Read by the orchestrator before each
    /// `run_agent_step` call so `/effort high` takes effect immediately.
    pub reasoning_effort: Arc<Mutex<Option<ReasoningEffort>>>,

    /// Active JSON output schema, if set. Read by the orchestrator
    /// before each `run_agent_step` call so `/schema {...}` takes
    /// effect on the next turn.
    pub output_schema: Arc<Mutex<Option<Value>>>,

    /// Live session name, mutated by `/name`. The on-disk index entry
    /// is updated separately by the `/name` closure when persistence is
    /// enabled.
    pub session_name: Arc<Mutex<Option<String>>>,

    /// Cumulative token usage across all completed agent steps in this
    /// session. Updated by the orchestrator after every successful
    /// `run_agent_step` and read by `/session`.
    pub cumulative_usage: Arc<Mutex<Usage>>,

    /// Session ID, when persistence is enabled. `None` for `--no-session`
    /// invocations.
    pub session_id: Option<String>,

    /// Filesystem location of the session JSONL store. Read by `/name`
    /// when it persists the new name through
    /// [`update_index_entry`](crate::session::update_index_entry).
    pub data_dir: PathBuf,

    /// Whether `--no-session` is active. When `true`, `/name` skips the
    /// index update.
    pub no_session: bool,

    /// The resolved session index-lock acquisition deadline
    /// (`resolve_index_lock_deadline`) applied to every lock-taking
    /// [`SessionManager`](crate::session::SessionManager) a slash
    /// handler constructs — `/name`'s index rename in particular.
    /// Without it a wedged sibling process would hang the running
    /// interactive surface forever inside the handler.
    pub index_lock_deadline: Duration,

    /// Raw `--variables KEY=VALUE` pairs in original order. `/variables`
    /// renders them verbatim; this side-steps the
    /// [`VariableStore`](norn::integration::variables::VariableStore)
    /// async-only accessor and matches the brief's R10 enrichment.
    pub variable_pairs: Vec<(String, String)>,

    /// Snapshot of `(tool_name, tool_description)` pairs taken at
    /// runtime-bundle construction. `/tools` iterates this list rather
    /// than re-reading the registry so the closure stays synchronous and
    /// free of lock contention.
    pub tools_snapshot: Arc<Vec<(String, String)>>,

    /// Snapshot of `(command_name, description)` rows used by `/help`.
    /// CLI builtins have a fixed shared-catalog description; profile-registered
    /// commands appear with the placeholder string `"(profile)"`.
    pub command_descriptions: Arc<Mutex<Vec<(String, String)>>>,

    /// Live, in-memory event store. Wrapped in
    /// `Arc<Mutex<Arc<EventStore>>>` so the orchestrator can swap the
    /// inner [`Arc<EventStore>`] when `/clear` fires — handler closures
    /// re-read the inner Arc on every invocation so `/session` always
    /// sees the latest store.
    pub store: Arc<Mutex<Arc<EventStore>>>,

    /// True when `/compact` has been requested. The orchestrator clears
    /// the flag after performing the compaction.
    pub compact_requested: Arc<AtomicBool>,

    /// True when `/clear` has been requested. The orchestrator clears
    /// the flag after replacing [`store`](Self::store) with a fresh
    /// [`EventStore`].
    pub clear_requested: Arc<AtomicBool>,

    /// True when `/exit` or `/quit` has been requested. The REPL
    /// orchestrator breaks out of its main loop on the next iteration.
    pub exit_requested: Arc<AtomicBool>,
}

impl SlashState {
    /// Construct a [`SlashState`] from the bundle-derived seed values.
    ///
    /// `tools` is a snapshot of `(name, description)` pairs collected
    /// from the gated tool registry at runtime-bundle construction.
    #[must_use]
    pub fn new(seed: SlashStateSeed) -> Self {
        Self {
            model: Arc::new(Mutex::new(seed.model)),
            service_tier: Arc::new(Mutex::new(seed.service_tier)),
            reasoning_effort: Arc::new(Mutex::new(seed.reasoning_effort)),
            output_schema: Arc::new(Mutex::new(seed.output_schema)),
            session_name: Arc::new(Mutex::new(seed.session_name)),
            cumulative_usage: Arc::new(Mutex::new(Usage::default())),
            session_id: seed.session_id,
            data_dir: seed.data_dir,
            no_session: seed.no_session,
            index_lock_deadline: seed.index_lock_deadline,
            variable_pairs: seed.variable_pairs,
            tools_snapshot: Arc::new(seed.tools),
            command_descriptions: Arc::new(Mutex::new(Vec::new())),
            store: Arc::new(Mutex::new(seed.store)),
            compact_requested: Arc::new(AtomicBool::new(false)),
            clear_requested: Arc::new(AtomicBool::new(false)),
            exit_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Snapshot the current event store [`Arc`].
    ///
    /// Closures use this to read the live store without holding the
    /// outer mutex across the closure body.
    #[must_use]
    pub fn current_store(&self) -> Arc<EventStore> {
        Arc::clone(&self.store.lock())
    }

    /// Replace the live event store with `new_store`. Called by the
    /// orchestrator after observing [`clear_requested`](Self::clear_requested).
    pub fn replace_store(&self, new_store: Arc<EventStore>) {
        *self.store.lock() = new_store;
    }

    /// Snapshot the active model identifier.
    #[must_use]
    pub fn model_snapshot(&self) -> String {
        self.model.lock().clone()
    }

    /// Snapshot the active service tier.
    #[must_use]
    pub fn service_tier_snapshot(&self) -> Option<ServiceTier> {
        *self.service_tier.lock()
    }

    /// Snapshot the active reasoning effort.
    #[must_use]
    pub fn reasoning_effort_snapshot(&self) -> Option<ReasoningEffort> {
        *self.reasoning_effort.lock()
    }

    /// Snapshot the active output schema.
    #[must_use]
    pub fn output_schema_snapshot(&self) -> Option<Value> {
        self.output_schema.lock().clone()
    }

    /// Snapshot the active session name.
    #[must_use]
    pub fn session_name_snapshot(&self) -> Option<String> {
        self.session_name.lock().clone()
    }

    /// Snapshot cumulative usage.
    #[must_use]
    pub fn cumulative_usage_snapshot(&self) -> Usage {
        self.cumulative_usage.lock().clone()
    }

    /// Add `delta` to the cumulative usage tally.
    pub fn add_usage(&self, delta: Usage) {
        let mut guard = self.cumulative_usage.lock();
        *guard = std::mem::take(&mut *guard) + delta;
    }
}

/// Inputs to [`SlashState::new`].
///
/// Groups the seed values to keep the constructor's argument count
/// within clippy's `too_many_arguments` budget and to give every input
/// a documented name.
pub struct SlashStateSeed {
    /// Initial active model identifier (from the bundle).
    pub model: String,
    /// Initial service tier (from the bundle loop context).
    pub service_tier: Option<ServiceTier>,
    /// Initial reasoning effort (from the bundle loop context).
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Initial output schema parsed from `-s/--output-schema`, if any.
    pub output_schema: Option<Value>,
    /// Initial session name from the index entry or `--session-name`.
    pub session_name: Option<String>,
    /// Session ID when persistence is enabled, `None` for `--no-session`.
    pub session_id: Option<String>,
    /// Filesystem root for session JSONL persistence.
    pub data_dir: PathBuf,
    /// True when `--no-session` was supplied.
    pub no_session: bool,
    /// The resolved session index-lock acquisition deadline applied to
    /// every lock-taking `SessionManager` a slash handler constructs.
    pub index_lock_deadline: Duration,
    /// Raw `--variables KEY=VALUE` pairs in original order.
    pub variable_pairs: Vec<(String, String)>,
    /// Snapshot of `(tool_name, description)` pairs from the gated
    /// tool registry.
    pub tools: Vec<(String, String)>,
    /// Initial event store [`Arc`]. The slash machinery keeps this
    /// behind an additional mutex so `/clear` can swap it.
    pub store: Arc<EventStore>,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn seed_with_store(store: Arc<EventStore>) -> SlashStateSeed {
        SlashStateSeed {
            model: "gpt-x".to_owned(),
            service_tier: None,
            reasoning_effort: None,
            output_schema: None,
            session_name: None,
            session_id: None,
            data_dir: PathBuf::from("/tmp/norn-cli-test"),
            no_session: true,
            // Test configuration: generous bound, never contended here.
            index_lock_deadline: Duration::from_secs(10),
            variable_pairs: Vec::new(),
            tools: Vec::new(),
            store,
        }
    }

    #[test]
    fn model_snapshot_returns_seed_value() {
        let state = SlashState::new(seed_with_store(Arc::new(EventStore::new())));
        assert_eq!(state.model_snapshot(), "gpt-x");
    }

    #[test]
    fn current_store_returns_initial_arc() {
        let store = Arc::new(EventStore::new());
        let state = SlashState::new(seed_with_store(Arc::clone(&store)));
        assert!(Arc::ptr_eq(&store, &state.current_store()));
    }

    #[test]
    fn replace_store_swaps_inner_arc() {
        let initial = Arc::new(EventStore::new());
        let state = SlashState::new(seed_with_store(Arc::clone(&initial)));
        let fresh = Arc::new(EventStore::new());
        state.replace_store(Arc::clone(&fresh));
        assert!(Arc::ptr_eq(&fresh, &state.current_store()));
        assert!(!Arc::ptr_eq(&initial, &state.current_store()));
    }

    #[test]
    fn cumulative_usage_starts_at_zero_and_accumulates() {
        let state = SlashState::new(seed_with_store(Arc::new(EventStore::new())));
        let snapshot = state.cumulative_usage_snapshot();
        assert_eq!(snapshot.input_tokens, 0);
        assert_eq!(snapshot.output_tokens, 0);

        state.add_usage(Usage {
            input_tokens: 10,
            output_tokens: 4,
            ..Usage::default()
        });
        state.add_usage(Usage {
            input_tokens: 5,
            output_tokens: 1,
            ..Usage::default()
        });

        let total = state.cumulative_usage_snapshot();
        assert_eq!(total.input_tokens, 15);
        assert_eq!(total.output_tokens, 5);
    }

    #[test]
    fn action_flags_initialised_to_false() {
        let state = SlashState::new(seed_with_store(Arc::new(EventStore::new())));
        assert!(
            !state
                .compact_requested
                .load(std::sync::atomic::Ordering::Relaxed)
        );
        assert!(
            !state
                .clear_requested
                .load(std::sync::atomic::Ordering::Relaxed)
        );
        assert!(
            !state
                .exit_requested
                .load(std::sync::atomic::Ordering::Relaxed)
        );
    }
}
