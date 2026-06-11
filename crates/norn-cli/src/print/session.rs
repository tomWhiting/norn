//! Session resolution for print-mode execution.
//!
//! Mirrors the TUI driver's session handling: honor `--no-session`,
//! `--resume`, `--fork`, and `--session-name`, returning an event store
//! (with a write-through sink attached when persisting) plus the index
//! entry the orchestrator appends to. Extracted from `orchestrator.rs`
//! so that module stays within the 500-line budget.

use std::sync::Arc;

use norn::session::store::{DurabilityPolicy, EventStore};

use crate::cli::Cli;
use crate::config::session_data_dir;
use crate::runtime::RuntimeBundle;
use crate::session::{
    SessionIndexEntry, attach_sink, create_session, fork_session, resume_session,
};

use super::orchestrator::PrintError;

/// Combined session-resolution state: an event store (fresh or
/// pre-populated) plus the index entry tracking the on-disk record.
///
/// `store` is wrapped in [`Arc`] so the slash-command machinery
/// ([`crate::commands::slash`]) can share read access without taking
/// the store out of the orchestrator. `&*store` derefs back to
/// `&EventStore` for the `run_agent_step` call site.
pub(crate) struct SessionHandle {
    pub(crate) store: Arc<EventStore>,
    pub(crate) entry: Option<SessionIndexEntry>,
}

impl SessionHandle {
    pub(crate) fn id(&self) -> Option<&str> {
        self.entry.as_ref().map(|e| e.id.as_str())
    }
}

fn working_dir_string() -> Result<String, PrintError> {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|err| PrintError::Io(format!("failed to determine working directory: {err}")))
}

/// Resolve the print-mode session, honoring `--no-session`, `--resume`,
/// `--fork`, and `--session-name`.
///
/// - `--no-session`: a fresh in-memory [`EventStore`] with no sink and no
///   on-disk record.
/// - `--resume <id>`: replay the persisted session into a store and
///   attach a write-through sink for continued appends.
/// - `--fork <id>`: copy the source session (with its `Fork` marker) into
///   a new one and attach a sink to the new file.
/// - otherwise: create a fresh persisted session, honoring
///   `--session-name`.
///
/// Every attached sink is index-registered via
/// [`attach_sink`]: each persisted event also updates the session's
/// `index.jsonl` entry (event count, token totals, `updated_at`), so no
/// caller reconciles the index by hand.
///
/// # Errors
///
/// Returns [`PrintError::Session`] when a resume / fork source cannot be
/// resolved or read or the new index entry cannot be written, and
/// [`PrintError::Io`] when the working directory cannot be determined.
pub(crate) fn open_session(cli: &Cli, bundle: &RuntimeBundle) -> Result<SessionHandle, PrintError> {
    let data_dir = session_data_dir();
    if cli.no_session {
        return Ok(SessionHandle {
            store: Arc::new(EventStore::new()),
            entry: None,
        });
    }

    let working_dir = working_dir_string()?;

    if let Some(id) = cli.resume.as_deref() {
        let (store, _events, entry) = resume_session(&data_dir, id)?;
        let store = attach_sink(store, &data_dir, &entry.id, DurabilityPolicy::Flush)?;
        return Ok(SessionHandle {
            store: Arc::new(store),
            entry: Some(entry),
        });
    }

    if let Some(id) = cli.fork.as_deref() {
        let (entry, store, _events) =
            fork_session(&data_dir, id, bundle.model.clone(), working_dir)?;
        let store = attach_sink(store, &data_dir, &entry.id, DurabilityPolicy::Flush)?;
        return Ok(SessionHandle {
            store: Arc::new(store),
            entry: Some(entry),
        });
    }

    let entry = create_session(
        &data_dir,
        bundle.model.clone(),
        working_dir,
        cli.session_name.clone(),
    )?;
    let store = attach_sink(
        EventStore::new(),
        &data_dir,
        &entry.id,
        DurabilityPolicy::Flush,
    )?;
    Ok(SessionHandle {
        store: Arc::new(store),
        entry: Some(entry),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
mod tests {
    use clap::Parser;
    use norn::session::events::{EventBase, EventUsage, SessionEvent};

    use crate::config::session_data_dir;
    use crate::runtime::{RuntimeInputs, build_runtime};
    use crate::session::read_index;

    use super::*;

    /// Set `NORN_HOME` to a temp directory for the duration of a test.
    /// Paired with `#[serial_test::serial]` on every consumer so no
    /// concurrent reader observes the mutated env.
    struct TempNornHome {
        prior: Option<std::ffi::OsString>,
        _tempdir: tempfile::TempDir,
    }

    impl TempNornHome {
        fn new(tempdir: tempfile::TempDir) -> Self {
            let prior = std::env::var_os("NORN_HOME");
            // SAFETY: paired with the `#[serial]` markers on every consumer;
            // no concurrent reader observes the mutated env.
            unsafe { std::env::set_var("NORN_HOME", tempdir.path()) };
            Self {
                prior,
                _tempdir: tempdir,
            }
        }
    }

    impl Drop for TempNornHome {
        fn drop(&mut self) {
            match &self.prior {
                Some(val) => unsafe { std::env::set_var("NORN_HOME", val) },
                None => unsafe { std::env::remove_var("NORN_HOME") },
            }
        }
    }

    fn assistant_with_usage(content: &str, input: u64, output: u64) -> SessionEvent {
        SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: content.to_owned(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            usage: EventUsage {
                input_tokens: input,
                output_tokens: output,
                ..EventUsage::default()
            },
            stop_reason: String::new(),
            response_id: None,
        }
    }

    /// Regression for the print-mode index double-count: the sink that
    /// `open_session` attaches is index-registered, so the events of a
    /// turn are counted in `index.jsonl` exactly once — by the sink, with
    /// no hand-reconcile on top. Pre-fix, the orchestrator re-added every
    /// turn's event count and token usage after `run_agent_step`,
    /// doubling both.
    #[test]
    #[serial_test::serial]
    fn print_session_sink_counts_index_fields_exactly_once() {
        let _guard = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = Cli::try_parse_from(["norn"]).unwrap();
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let session = open_session(&cli, &bundle).unwrap();
        let entry_id = session
            .id()
            .expect("persisted session has an id")
            .to_owned();

        // Simulate the events run_agent_step appends during one turn.
        session
            .store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "prompt".to_owned(),
            })
            .unwrap();
        session
            .store
            .append(assistant_with_usage("answer", 100, 40))
            .unwrap();

        // Mirror the orchestrator's post-turn flush: the registered sink
        // batches the index delta until checkpoint/drop.
        session.store.checkpoint().unwrap();

        let index = read_index(&session_data_dir()).unwrap();
        let indexed = index
            .iter()
            .find(|e| e.id == entry_id)
            .expect("session present in index");
        assert_eq!(
            indexed.event_count, 2,
            "each persisted event must be counted exactly once",
        );
        assert_eq!(
            indexed.total_input_tokens, 100,
            "usage totals must be summed exactly once",
        );
        assert_eq!(indexed.total_output_tokens, 40);
    }

    /// `--no-session` opens a sink-less in-memory store and records
    /// nothing on disk.
    #[test]
    #[serial_test::serial]
    fn no_session_opens_unpersisted_store() {
        let _guard = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = Cli::try_parse_from(["norn", "--no-session"]).unwrap();
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let session = open_session(&cli, &bundle).unwrap();
        assert!(session.entry.is_none());
        session
            .store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "x".to_owned(),
            })
            .unwrap();
        let index = read_index(&session_data_dir()).unwrap();
        assert!(index.is_empty(), "--no-session must write no index entry");
    }
}
