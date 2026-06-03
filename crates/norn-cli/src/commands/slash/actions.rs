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
//! summing each superseded event's content through the bundle's
//! [`TokenEstimator`](norn::r#loop::tokens::TokenEstimator).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use norn::session::events::SessionEvent;
use norn::session::store::EventStore;

use crate::runtime::RuntimeBundle;
use crate::session::{SessionPersistError, sum_usage_from_events, update_session_index};

use super::state::SlashState;

/// Outcome of [`apply_compact_request`].
///
/// Surfaced so the orchestrator can report what compaction did. The
/// `Compaction` event is persisted by the attached write-through sink;
/// `apply_compact_request` only reconciles the session index afterwards.
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
    /// installed — should not happen with a runtime built via
    /// [`crate::runtime::build_runtime`], which always wires
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
/// Returns the outcome so the caller can decide what to log or persist.
/// Clears the flag regardless of outcome — `/compact` is a single-shot
/// signal even when the store has nothing to do.
///
/// # Errors
///
/// Returns [`SessionPersistError`] when the session index cannot be
/// reconciled after compaction. The `Compaction` event itself is
/// persisted by the attached write-through sink, not here.
pub fn apply_compact_request(
    bundle: &mut RuntimeBundle,
    store: &Arc<EventStore>,
    state: &SlashState,
    data_dir: &Path,
    session_id: Option<&str>,
    no_session: bool,
) -> Result<Option<CompactOutcome>, SessionPersistError> {
    if !state.compact_requested.swap(false, Ordering::Relaxed) {
        return Ok(None);
    }
    let keep = bundle.agent_config.auto_compact_keep_recent_turns;
    let events = store.events();
    let assistant_positions: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(idx, event)| {
            matches!(event, SessionEvent::AssistantMessage { .. }).then_some(idx)
        })
        .collect();
    if assistant_positions.len() <= keep {
        return Ok(Some(CompactOutcome::Nothing));
    }
    let cut_idx = assistant_positions[assistant_positions.len() - keep - 1];
    let event_count = cut_idx + 1;
    let token_estimate_freed = estimate_freed(bundle, &events[..=cut_idx]);

    let Some(edits) = bundle.loop_context.context_edits.as_mut() else {
        return Ok(Some(CompactOutcome::ContextEditsUnavailable));
    };

    let pre_event_count = store.len();
    match edits.auto_compact_keeping_recent_turns(store, keep, token_estimate_freed) {
        Ok(Some(_)) => {
            // The `Compaction` event was written through the attached
            // `JsonlSink` already; calling `append_events` here would
            // write it a second time and break resume on the
            // duplicate-ID guard. Only reconcile the index.
            if let Some(id) = session_id
                && !no_session
            {
                let all = store.events();
                if all.len() > pre_event_count {
                    let new_events = &all[pre_event_count..];
                    let appended = u64::try_from(new_events.len()).unwrap_or(u64::MAX);
                    let usage_delta = sum_usage_from_events(new_events);
                    update_session_index(data_dir, id, appended, &usage_delta)?;
                }
            }
            Ok(Some(CompactOutcome::Performed {
                compacted_events: event_count,
                token_estimate_freed,
            }))
        }
        Ok(None) => Ok(Some(CompactOutcome::Nothing)),
        Err(err) => Err(err.into()),
    }
}

/// Apply the `/clear` flag if it is set. The on-disk JSONL is left
/// untouched — only the in-memory event store is replaced.
pub fn apply_clear_request(state: &SlashState) -> bool {
    if state.clear_requested.swap(false, Ordering::Relaxed) {
        state.replace_store(Arc::new(EventStore::new()));
        true
    } else {
        false
    }
}

fn estimate_freed(bundle: &RuntimeBundle, events: &[SessionEvent]) -> usize {
    let Some(estimator) = bundle.loop_context.token_estimator.as_ref() else {
        return 0;
    };
    let mut total: usize = 0;
    for event in events {
        let bytes = match event {
            SessionEvent::UserMessage { content, .. } => estimator.estimate(content),
            SessionEvent::AssistantMessage { content, .. } => {
                if content.is_empty() {
                    0
                } else {
                    estimator.estimate(content)
                }
            }
            SessionEvent::ToolResult { output, .. } => estimator.estimate(&output.to_string()),
            SessionEvent::SpokenResponse { content, .. } => {
                estimator.estimate(&content.to_string())
            }
            SessionEvent::Compaction { summary, .. } => estimator.estimate(summary),
            SessionEvent::ModelChange { .. }
            | SessionEvent::Fork { .. }
            | SessionEvent::ForkComplete { .. }
            | SessionEvent::Label { .. }
            | SessionEvent::Custom { .. } => 0,
        };
        total = total.saturating_add(bytes);
    }
    total
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser;
    use norn::session::events::{EventBase, EventUsage};
    use norn::session::store::EventStore;

    use crate::cli::Cli;
    use crate::commands::slash::state::{SlashState, SlashStateSeed};
    use crate::runtime::RuntimeInputs;
    use crate::runtime::build_runtime;
    use crate::session::{attach_sink, create_session, resume_session, session_file_path};

    use super::*;

    fn make_state(store: Arc<EventStore>) -> SlashState {
        SlashState::new(SlashStateSeed {
            model: "gpt-x".to_owned(),
            output_schema: None,
            session_name: None,
            session_id: None,
            data_dir: PathBuf::from("/tmp/norn-cli-slash-actions"),
            no_session: true,
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
            tool_calls: Vec::new(),
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        }
    }

    #[test]
    fn compact_flag_not_set_returns_none() {
        let store = Arc::new(EventStore::new());
        let mut bundle = build_runtime(
            &Cli::try_parse_from(["norn"]).unwrap(),
            RuntimeInputs::default(),
        )
        .unwrap();
        let state = make_state(Arc::clone(&store));
        let outcome =
            apply_compact_request(&mut bundle, &store, &state, Path::new("/tmp"), None, true)
                .unwrap();
        assert!(outcome.is_none());
    }

    #[test]
    fn compact_on_empty_store_reports_nothing() {
        let store = Arc::new(EventStore::new());
        let mut bundle = build_runtime(
            &Cli::try_parse_from(["norn"]).unwrap(),
            RuntimeInputs::default(),
        )
        .unwrap();
        let state = make_state(Arc::clone(&store));
        state.compact_requested.store(true, Ordering::Relaxed);
        let outcome =
            apply_compact_request(&mut bundle, &store, &state, Path::new("/tmp"), None, true)
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
        let mut bundle = build_runtime(
            &Cli::try_parse_from(["norn"]).unwrap(),
            RuntimeInputs::default(),
        )
        .unwrap();
        let state = make_state(Arc::clone(&store));
        state.compact_requested.store(true, Ordering::Relaxed);
        let outcome =
            apply_compact_request(&mut bundle, &store, &state, Path::new("/tmp"), None, true)
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
        // is written through the attached sink, and apply_compact_request
        // must only reconcile the index — not re-append the event.
        let tmp = tempfile::tempdir().unwrap();
        let entry =
            create_session(tmp.path(), "gpt-x".to_owned(), "/work".to_owned(), None).unwrap();
        // Write-through sink: every append lands in the session JSONL.
        let store = Arc::new(attach_sink(EventStore::new(), tmp.path(), &entry.id));
        for i in 0..12 {
            store.append(user(&format!("u{i}"))).unwrap();
            store.append(assistant(&format!("a{i}"))).unwrap();
        }
        let mut bundle = build_runtime(
            &Cli::try_parse_from(["norn"]).unwrap(),
            RuntimeInputs::default(),
        )
        .unwrap();
        let state = make_state(Arc::clone(&store));
        state.compact_requested.store(true, Ordering::Relaxed);

        let outcome = apply_compact_request(
            &mut bundle,
            &store,
            &state,
            tmp.path(),
            Some(&entry.id),
            false,
        )
        .unwrap();
        assert!(matches!(outcome, Some(CompactOutcome::Performed { .. })));

        // 24 turn events + exactly one Compaction event = 25 lines. A
        // double-write would yield 26 and a duplicate EventId.
        let body = std::fs::read_to_string(session_file_path(tmp.path(), &entry.id)).unwrap();
        let line_count = body.lines().filter(|l| !l.trim().is_empty()).count();
        assert_eq!(
            line_count, 25,
            "compaction event must be persisted once, not double-written"
        );

        // Resume succeeds — the duplicate-ID guard never fires.
        let (resumed, replayed, _) = resume_session(tmp.path(), &entry.id).unwrap();
        assert_eq!(resumed.len(), 25);
        assert_eq!(replayed.len(), 25);
    }

    #[test]
    fn clear_request_swaps_store_only_when_flag_is_set() {
        let store = Arc::new(EventStore::new());
        store.append(user("first")).unwrap();
        let state = make_state(Arc::clone(&store));
        assert!(!apply_clear_request(&state));
        // store unchanged
        assert_eq!(state.current_store().len(), 1);

        state.clear_requested.store(true, Ordering::Relaxed);
        assert!(apply_clear_request(&state));
        assert_eq!(state.current_store().len(), 0);
        // original Arc kept by caller is not affected.
        assert_eq!(store.len(), 1);
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
