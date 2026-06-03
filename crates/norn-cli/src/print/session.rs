//! Session resolution for print-mode execution.
//!
//! Mirrors the TUI driver's session handling: honor `--no-session`,
//! `--resume`, `--fork`, and `--session-name`, returning an event store
//! (with a write-through sink attached when persisting) plus the index
//! entry the orchestrator appends to. Extracted from `orchestrator.rs`
//! so that module stays within the 500-line budget.

use std::path::PathBuf;
use std::sync::Arc;

use norn::session::store::EventStore;

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
/// # Errors
///
/// Returns [`PrintError::Session`] when a resume / fork source cannot be
/// resolved or read or the new index entry cannot be written, and
/// [`PrintError::Io`] when the working directory cannot be determined.
pub(crate) fn open_session(
    cli: &Cli,
    bundle: &RuntimeBundle,
) -> Result<(SessionHandle, PathBuf), PrintError> {
    let data_dir = session_data_dir();
    if cli.no_session {
        return Ok((
            SessionHandle {
                store: Arc::new(EventStore::new()),
                entry: None,
            },
            data_dir,
        ));
    }

    let working_dir = working_dir_string()?;

    if let Some(id) = cli.resume.as_deref() {
        let (store, _events, entry) = resume_session(&data_dir, id)?;
        let store = attach_sink(store, &data_dir, &entry.id);
        return Ok((
            SessionHandle {
                store: Arc::new(store),
                entry: Some(entry),
            },
            data_dir,
        ));
    }

    if let Some(id) = cli.fork.as_deref() {
        let (entry, store, _events) =
            fork_session(&data_dir, id, bundle.model.clone(), working_dir)?;
        let store = attach_sink(store, &data_dir, &entry.id);
        return Ok((
            SessionHandle {
                store: Arc::new(store),
                entry: Some(entry),
            },
            data_dir,
        ));
    }

    let entry = create_session(
        &data_dir,
        bundle.model.clone(),
        working_dir,
        cli.session_name.clone(),
    )?;
    let store = attach_sink(EventStore::new(), &data_dir, &entry.id);
    Ok((
        SessionHandle {
            store: Arc::new(store),
            entry: Some(entry),
        },
        data_dir,
    ))
}
