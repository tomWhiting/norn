//! Action-flag handling for the print orchestrator (NC-006 R6, R7).
//!
//! `/compact` and `/clear` set bits on [`SlashState`] inside their
//! closures; the orchestrator drains those flags and applies the
//! effects here so the closures stay synchronous and free of
//! `&mut LoopContext` access. `/exit` is intentionally NOT handled in
//! this module — the orchestrator checks the flag separately because
//! the exit signal short-circuits the rest of the dispatch flow.
//!
//! The exact compaction calculation mirrors libnorn's
//! [`ContextEdits::auto_compact_keeping_recent_turns`](norn::session::context_edit::ContextEdits::auto_compact_keeping_recent_turns):
//! count assistant turns, locate the cut index that retains the most
//! recent `keep` turns, then estimate the tokens that will be freed by
//! summing each superseded event's content through the loop context's
//! [`TokenEstimator`](norn::agent_loop::tokens::TokenEstimator).

use std::sync::Arc;
use std::sync::atomic::Ordering;

use norn::agent_loop::loop_context::LoopContext;
use norn::session::store::EventStore;

use crate::session::{CreateSessionOptions, SessionManager, SessionPersistError};

use super::state::SlashState;

/// Outcome of [`apply_compact_request`].
///
/// Surfaced so the orchestrator can report what compaction did. The
/// `Compaction` event is persisted — and the session index updated — by
/// the attached index-registered write-through sink; this module touches
/// neither the JSONL nor the index itself.
#[must_use]
pub enum CompactOutcome {
    /// Compaction completed; the slice covers the events newly appended
    /// to the store as a result of the operation.
    Performed {
        /// Number of events that were superseded by the compaction.
        compacted_events: usize,
        /// Token-estimate metadata recorded on the `Compaction` event.
        token_estimate_freed: usize,
    },
    /// The store did not have enough assistant turns to compact.
    Nothing,
    /// Compaction was requested but `LoopContext::context_edits` is not
    /// installed — should not happen with an agent assembled through
    /// `AgentBuilder`, whose `load_runtime_base` always wires
    /// [`ContextEdits`](norn::session::context_edit::ContextEdits).
    ContextEditsUnavailable,
}

impl CompactOutcome {
    /// Render a one-line summary to stderr.
    pub fn log_to_stderr(&self) {
        match self {
            Self::Performed {
                compacted_events,
                token_estimate_freed,
            } => {
                eprintln!(
                    "Compacted {compacted_events} events, freed ~{token_estimate_freed} tokens."
                );
            }
            Self::Nothing => eprintln!("Nothing to compact."),
            Self::ContextEditsUnavailable => {
                eprintln!("norn: warning: context edits unavailable; cannot compact.");
            }
        }
    }
}

/// Apply the `/compact` flag if it is set.
///
/// Returns the outcome so the caller can decide what to log. Clears the
/// flag regardless of outcome — `/compact` is a single-shot signal even
/// when the store has nothing to do.
///
/// Persistence is entirely the store's concern: the `Compaction` event
/// is written through the attached index-registered sink, which also
/// updates the session's `index.jsonl` entry. Reconciling the index here
/// on top of that would double-count it (the pre-fix print-mode bug).
///
/// # Errors
///
/// Returns [`SessionPersistError`] when appending the `Compaction` event
/// through `auto_compact_keeping_recent_turns` fails.
pub fn apply_compact_request(
    keep: usize,
    loop_context: &mut LoopContext,
    store: &Arc<EventStore>,
    state: &SlashState,
) -> Result<Option<CompactOutcome>, SessionPersistError> {
    if !state.compact_requested.swap(false, Ordering::Relaxed) {
        return Ok(None);
    }
    let Some(estimate) = norn::agent_loop::estimate_manual_compaction(
        store,
        keep,
        loop_context.token_estimator.as_deref(),
    ) else {
        return Ok(Some(CompactOutcome::Nothing));
    };

    let Some(edits) = loop_context.context_edits.as_mut() else {
        return Ok(Some(CompactOutcome::ContextEditsUnavailable));
    };

    match edits.auto_compact_keeping_recent_turns(store, keep, estimate.token_estimate_freed) {
        Ok(Some(_)) => Ok(Some(CompactOutcome::Performed {
            compacted_events: estimate.compacted_events,
            token_estimate_freed: estimate.token_estimate_freed,
        })),
        Ok(None) => Ok(Some(CompactOutcome::Nothing)),
        Err(err) => Err(err.into()),
    }
}

/// Outcome of [`apply_clear_request`].
#[derive(Debug)]
#[must_use]
pub enum ClearOutcome {
    /// `--no-session` invocation: the store was replaced with a fresh
    /// sink-less in-memory store — the caller's explicit no-persistence
    /// choice propagates across `/clear`.
    ClearedInMemory,
    /// Persisted invocation: the slash state rotated into a fresh
    /// sink-registered session, so post-clear appends are exactly as
    /// durable as pre-clear ones (session-fidelity Gap 12).
    RotatedToNewSession {
        /// ID of the freshly created session now behind
        /// [`SlashState::current_store`].
        new_session_id: String,
    },
}

/// Apply the `/clear` flag if it is set.
///
/// The pre-clear session's JSONL is left untouched (append-only,
/// already durable). What replaces the store depends on the invocation
/// (session-fidelity Gap 12 — the post-clear store must carry the same
/// sink discipline as the pre-clear one):
///
/// - `--no-session`: a fresh sink-less [`EventStore`] — the explicit
///   no-persistence choice propagates.
/// - persisted session: a **new** session is created through
///   [`SessionManager`] (same model — the live `/model` value — and the
///   retired session's working directory) and its sink-registered store
///   is swapped in; [`SlashState::session_id`] is rotated to the new ID
///   and the live session name resets (the old name belongs to the
///   retired session's index entry). This mirrors the TUI's `/new`
///   rotation: all fallible work happens before any state is touched,
///   so a failure leaves the pre-clear state fully intact — never a
///   silent fallback to a memory-only store.
///
/// The retired store's sink flushes its pending index delta when its
/// last `Arc` drops (checkpointed earlier by the orchestrator's normal
/// post-turn flow); its events were already written through per append.
///
/// # Errors
///
/// [`SessionPersistError`] when the retired session cannot be resolved
/// in the index or the replacement session cannot be created. The flag
/// is consumed either way (single-shot signal, matching `/compact`),
/// and the store/session cells are untouched on error.
pub fn apply_clear_request(
    state: &SlashState,
    durability: norn::session::DurabilityPolicy,
) -> Result<Option<ClearOutcome>, SessionPersistError> {
    if !state.clear_requested.swap(false, Ordering::Relaxed) {
        return Ok(None);
    }
    let old_session_id = state.current_session_id();
    let (Some(old_session_id), false) = (old_session_id, state.no_session) else {
        state.replace_store(Arc::new(EventStore::new()));
        return Ok(Some(ClearOutcome::ClearedInMemory));
    };
    // Fallible work first, state mutation last (the TUI rotation's
    // no-partially-rotated-state contract): resolve the retired entry
    // for its working directory, then create the replacement session.
    // The index-lock wait is bounded by the CLI's resolved deadline —
    // creating the replacement session rewrites the index under the
    // inter-process lock, and a wedged sibling must not freeze `/clear`.
    let manager = SessionManager::new(&state.data_dir)
        .with_index_lock_deadline(Some(state.index_lock_deadline));
    let old_entry = norn::session::resolve_session(&state.data_dir, &old_session_id)?;
    let opened = manager.create(
        CreateSessionOptions {
            model: state.model.lock().clone(),
            working_dir: old_entry.working_dir,
            name: None,
        },
        durability,
    )?;
    let new_session_id = opened.entry.id.clone();
    state.replace_store(Arc::new(opened.store));
    *state.session_id.lock() = Some(new_session_id.clone());
    *state.session_name.lock() = None;
    Ok(Some(ClearOutcome::RotatedToNewSession { new_session_id }))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::PathBuf;

    use norn::agent::{AgentBuilder, AgentParts};
    use norn::provider::mock::MockProvider;
    use norn::provider::traits::Provider;
    use norn::session::events::{EventBase, EventUsage, SessionEvent};
    use norn::session::store::EventStore;

    use crate::commands::slash::state::{SlashState, SlashStateSeed};
    use crate::session::{CreateSessionOptions, SessionManager, session_file_path};

    use super::*;

    /// Assemble a headless agent through the library builder and hand back
    /// its parts, so the compact tests read the same
    /// `auto_compact_keep_recent_turns`, `token_estimator`, and
    /// `context_edits` the print orchestrator's `AgentParts` carry.
    fn built_parts() -> AgentParts {
        let provider: Arc<dyn Provider> = Arc::new(MockProvider::new(Vec::new()));
        AgentBuilder::new(provider)
            .model("gpt-x")
            // Explicit window: "gpt-x" is deliberately uncatalogued and
            // `build` hard-errors on an unarmed window (2026-07-05
            // incident guard) — without this the fixture only passed
            // when the developer's own ~/.norn settings happened to
            // supply one. `272_000` is gpt-5.5's catalogued standard
            // window (assets/models.json) — factual, not invented; same
            // fixture convention as the library builder tests.
            .context_window_limit(272_000)
            .working_dir(std::env::temp_dir())
            .load_runtime_base()
            .build()
            .expect("build succeeds")
            .into_parts()
    }

    /// Open a fresh sink-backed session for the compact regression
    /// tests, mirroring what the orchestrator gets from `open_session`.
    fn open_persisted(dir: &std::path::Path) -> (String, Arc<EventStore>) {
        let opened = SessionManager::new(dir)
            .create(
                CreateSessionOptions {
                    model: "gpt-x".to_owned(),
                    working_dir: "/work".to_owned(),
                    name: None,
                },
                norn::session::DurabilityPolicy::Flush,
            )
            .expect("create session");
        (opened.entry.id, Arc::new(opened.store))
    }

    fn make_state(store: Arc<EventStore>) -> SlashState {
        SlashState::new(SlashStateSeed {
            model: "gpt-x".to_owned(),
            service_tier: None,
            reasoning_effort: None,
            output_schema: None,
            session_name: None,
            session_id: None,
            data_dir: PathBuf::from("/tmp/norn-cli-slash-actions"),
            no_session: true,
            // Test configuration: generous bound, never contended here.
            index_lock_deadline: std::time::Duration::from_secs(10),
            variable_pairs: Vec::new(),
            tools: Vec::new(),
            store,
        })
    }

    /// A persisted-session slash state, as print mode builds it when
    /// `--no-session` is absent.
    fn make_persisted_state(
        store: Arc<EventStore>,
        session_id: &str,
        data_dir: &std::path::Path,
    ) -> SlashState {
        SlashState::new(SlashStateSeed {
            model: "gpt-x".to_owned(),
            service_tier: None,
            reasoning_effort: None,
            output_schema: None,
            session_name: Some("original".to_owned()),
            session_id: Some(session_id.to_owned()),
            data_dir: data_dir.to_path_buf(),
            no_session: false,
            // Test configuration: generous bound, never contended here.
            index_lock_deadline: std::time::Duration::from_secs(10),
            variable_pairs: Vec::new(),
            tools: Vec::new(),
            store,
        })
    }

    fn user(content: &str) -> SessionEvent {
        SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
        }
    }

    fn assistant(content: &str) -> SessionEvent {
        SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }
    }

    #[test]
    fn compact_flag_not_set_returns_none() {
        let store = Arc::new(EventStore::new());
        let mut parts = built_parts();
        let state = make_state(Arc::clone(&store));
        let outcome = apply_compact_request(
            parts.config.auto_compact_keep_recent_turns,
            &mut parts.loop_context,
            &store,
            &state,
        )
        .unwrap();
        assert!(outcome.is_none());
    }

    #[test]
    fn compact_on_empty_store_reports_nothing() {
        let store = Arc::new(EventStore::new());
        let mut parts = built_parts();
        let state = make_state(Arc::clone(&store));
        state.compact_requested.store(true, Ordering::Relaxed);
        let outcome = apply_compact_request(
            parts.config.auto_compact_keep_recent_turns,
            &mut parts.loop_context,
            &store,
            &state,
        )
        .unwrap();
        assert!(matches!(outcome, Some(CompactOutcome::Nothing)));
        assert!(!state.compact_requested.load(Ordering::Relaxed));
    }

    #[test]
    fn compact_with_many_turns_supersedes_older_events() {
        // 12 user/assistant pairs => 12 assistant turns => keep=10 means
        // 2 oldest turns (4 events) get superseded.
        let store = Arc::new(EventStore::new());
        for i in 0..12 {
            store.append(user(&format!("u{i}"))).unwrap();
            store.append(assistant(&format!("a{i}"))).unwrap();
        }
        let mut parts = built_parts();
        let state = make_state(Arc::clone(&store));
        state.compact_requested.store(true, Ordering::Relaxed);
        let outcome = apply_compact_request(
            parts.config.auto_compact_keep_recent_turns,
            &mut parts.loop_context,
            &store,
            &state,
        )
        .unwrap();
        match outcome {
            Some(CompactOutcome::Performed {
                compacted_events, ..
            }) => {
                // cut_idx = assistant_positions[12 - 10 - 1] = position
                // of the 2nd assistant message in the store = index 3,
                // so event_count = cut_idx + 1 = 4.
                assert_eq!(compacted_events, 4);
            }
            other => panic!(
                "expected Performed, got {:?}",
                debug_outcome(other.as_ref())
            ),
        }
    }

    #[test]
    fn compact_persists_compaction_once_and_stays_resumable() {
        // Regression for the /compact double-write: the Compaction event
        // is written through the attached sink; apply_compact_request
        // must not re-append it (nor touch the index).
        let tmp = tempfile::tempdir().unwrap();
        // Write-through sink: every append lands in the session JSONL.
        let (entry_id, store) = open_persisted(tmp.path());
        for i in 0..12 {
            store.append(user(&format!("u{i}"))).unwrap();
            store.append(assistant(&format!("a{i}"))).unwrap();
        }
        let mut parts = built_parts();
        let state = make_state(Arc::clone(&store));
        state.compact_requested.store(true, Ordering::Relaxed);

        let outcome = apply_compact_request(
            parts.config.auto_compact_keep_recent_turns,
            &mut parts.loop_context,
            &store,
            &state,
        )
        .unwrap();
        assert!(matches!(outcome, Some(CompactOutcome::Performed { .. })));

        // 24 turn events + exactly one Compaction event = 25 event
        // lines (the versioned session file carries one extra
        // `norn_session_format` header line, excluded here). A
        // double-write would yield 26 event lines and a duplicate
        // EventId.
        let body = std::fs::read_to_string(session_file_path(tmp.path(), &entry_id)).unwrap();
        let line_count = body
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.contains("norn_session_format"))
            .count();
        assert_eq!(
            line_count, 25,
            "compaction event must be persisted once, not double-written"
        );

        // Resume succeeds — the duplicate-ID guard never fires.
        let resumed = SessionManager::new(tmp.path())
            .resume(&entry_id, norn::session::DurabilityPolicy::Flush)
            .unwrap();
        assert_eq!(resumed.store.len(), 25);
        assert_eq!(resumed.replay.replayed_events, 25);
    }

    #[test]
    fn compact_counts_index_events_exactly_once() {
        // Regression for the /compact index double-count: the registered
        // write-through sink already updates index.jsonl per persisted
        // event, so apply_compact_request must NOT hand-reconcile the
        // index on top. Pre-fix this read 26 (25 sink updates + 1
        // duplicate reconcile for the Compaction event).
        let tmp = tempfile::tempdir().unwrap();
        let (entry_id, store) = open_persisted(tmp.path());
        for i in 0..12 {
            store.append(user(&format!("u{i}"))).unwrap();
            store.append(assistant(&format!("a{i}"))).unwrap();
        }
        let mut parts = built_parts();
        let state = make_state(Arc::clone(&store));
        state.compact_requested.store(true, Ordering::Relaxed);

        let outcome = apply_compact_request(
            parts.config.auto_compact_keep_recent_turns,
            &mut parts.loop_context,
            &store,
            &state,
        )
        .unwrap();
        assert!(matches!(outcome, Some(CompactOutcome::Performed { .. })));
        // Mirror the orchestrator: checkpoint flushes the sink's pending
        // index delta (the registered sink batches it until a durability
        // boundary, checkpoint, or drop).
        store.checkpoint().unwrap();

        let index = crate::session::read_index(tmp.path()).unwrap();
        let indexed = index
            .iter()
            .find(|e| e.id == entry_id)
            .expect("session present in index");
        assert_eq!(
            indexed.event_count, 25,
            "index must count each persisted event exactly once (24 turn \
             events + 1 Compaction event); a hand-reconcile on top of the \
             registered sink double-counts",
        );
    }

    #[test]
    fn clear_request_swaps_store_only_when_flag_is_set() {
        let store = Arc::new(EventStore::new());
        store.append(user("first")).unwrap();
        let state = make_state(Arc::clone(&store));
        assert!(
            apply_clear_request(&state, norn::session::DurabilityPolicy::Flush)
                .unwrap()
                .is_none()
        );
        // store unchanged
        assert_eq!(state.current_store().len(), 1);

        state.clear_requested.store(true, Ordering::Relaxed);
        let outcome = apply_clear_request(&state, norn::session::DurabilityPolicy::Flush)
            .unwrap()
            .expect("flag was set");
        assert!(
            matches!(outcome, ClearOutcome::ClearedInMemory),
            "--no-session keeps the explicit sink-less choice across /clear",
        );
        assert_eq!(state.current_store().len(), 0);
        // original Arc kept by caller is not affected.
        assert_eq!(store.len(), 1);
    }

    /// Gap 12 regression: on a persisted invocation, `/clear` must rotate
    /// into a fresh sink-registered session — events appended to the
    /// post-clear store land on disk, byte-for-byte replayable, exactly
    /// like pre-clear ones. A sink-less swap would pass every in-memory
    /// assertion and silently lose everything after the clear.
    #[test]
    fn clear_on_persisted_session_rotates_into_a_durable_store() {
        let tmp = tempfile::tempdir().unwrap();
        let (old_id, old_store) = open_persisted(tmp.path());
        old_store.append(user("pre-clear")).unwrap();
        let state = make_persisted_state(Arc::clone(&old_store), &old_id, tmp.path());

        state.clear_requested.store(true, Ordering::Relaxed);
        let outcome = apply_clear_request(&state, norn::session::DurabilityPolicy::Flush)
            .unwrap()
            .expect("flag was set");
        let ClearOutcome::RotatedToNewSession { new_session_id } = outcome else {
            panic!("persisted /clear must rotate, not fall back to memory-only");
        };
        assert_ne!(
            new_session_id, old_id,
            "a fresh session, never the retired one"
        );
        assert_eq!(
            state.current_session_id().as_deref(),
            Some(new_session_id.as_str()),
            "handler closures must observe the rotated session id",
        );
        assert!(
            state.session_name.lock().is_none(),
            "the retired session's name must not leak onto the new one",
        );
        assert_eq!(state.current_store().len(), 0, "conversation cleared");

        // The heart of Gap 12: post-clear appends reach disk.
        state.current_store().append(user("post-clear")).unwrap();
        let new_file =
            std::fs::read_to_string(session_file_path(tmp.path(), &new_session_id)).unwrap();
        assert!(
            new_file.contains("post-clear"),
            "post-clear events must be written through to the new session \
             file; got: {new_file}",
        );
        // And the retired session's file still holds only its own events.
        let old_file = std::fs::read_to_string(session_file_path(tmp.path(), &old_id)).unwrap();
        assert!(old_file.contains("pre-clear"));
        assert!(!old_file.contains("post-clear"));

        // The rotated session resumes cleanly through the manager.
        let resumed = SessionManager::new(tmp.path())
            .resume(&new_session_id, norn::session::DurabilityPolicy::Flush)
            .unwrap();
        assert_eq!(resumed.store.len(), 1);
    }

    /// A rotation failure must leave the pre-clear state fully intact —
    /// typed error out, no silent memory-only fallback, store and
    /// session id untouched.
    #[test]
    fn clear_rotation_failure_leaves_pre_clear_state_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(EventStore::new());
        store.append(user("kept")).unwrap();
        // A session id that is not in the (empty) index: resolution fails.
        let state = make_persisted_state(Arc::clone(&store), "missing-session", tmp.path());

        state.clear_requested.store(true, Ordering::Relaxed);
        let err = apply_clear_request(&state, norn::session::DurabilityPolicy::Flush)
            .expect_err("resolving the retired session must fail");
        assert!(
            matches!(err, SessionPersistError::NotFound { .. }),
            "expected NotFound, got {err:?}",
        );
        assert_eq!(
            state.current_store().len(),
            1,
            "the pre-clear store must remain live on error",
        );
        assert_eq!(
            state.current_session_id().as_deref(),
            Some("missing-session"),
            "the session id must not rotate on error",
        );
    }

    fn debug_outcome(outcome: Option<&CompactOutcome>) -> String {
        match outcome {
            None => "None".to_owned(),
            Some(CompactOutcome::Nothing) => "Nothing".to_owned(),
            Some(CompactOutcome::Performed {
                compacted_events,
                token_estimate_freed,
            }) => format!(
                "Performed {{ compacted_events: {compacted_events}, token_estimate_freed: {token_estimate_freed} }}"
            ),
            Some(CompactOutcome::ContextEditsUnavailable) => "ContextEditsUnavailable".to_owned(),
        }
    }
}
