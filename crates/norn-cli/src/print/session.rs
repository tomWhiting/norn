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
use crate::session::{CreateSessionOptions, OpenSession, SessionIndexEntry, SessionManager};

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

/// Resolve the print-mode session through [`SessionManager`], honoring
/// `--no-session`, `--resume`, `--fork`, `--session-id`,
/// `--resume-if-exists`, and `--session-name`.
///
/// - `--no-session`: a fresh in-memory [`EventStore`] with no sink and no
///   on-disk record.
/// - `--resume <id>`: replay the persisted session and continue appending
///   through its write-through sink. With no argument, resumes the most
///   recently updated session for the current working directory.
/// - `--fork <id>`: copy the source session (with its `Fork` marker) into
///   a new one. With no argument, forks the most recently updated session
///   for the current working directory.
/// - `--session-id <id>`: create a fresh persisted session under the
///   caller's exact ID — a typed failure when the ID already exists
///   unless `--resume-if-exists` is also supplied
///   (create-exactly-this; clap rejects combining it with
///   `--resume`/`--fork`/`--no-session`). Honors `--session-name` only
///   on the create arm.
/// - otherwise: create a fresh persisted session, honoring
///   `--session-name`.
///
/// Every store the manager returns carries an index-registered sink:
/// each persisted event also updates the session's `index.jsonl` entry
/// (event count, token totals, `updated_at`), so no caller reconciles
/// the index by hand. Lines the tolerant reader skipped during a resume
/// or fork replay are reported on stderr — a partial replay is never
/// silent.
///
/// # Errors
///
/// Returns [`PrintError::Session`] when a resume / fork source cannot be
/// resolved or read or the new index entry cannot be written, and
/// [`PrintError::Io`] when the working directory cannot be determined.
pub(crate) fn open_session(cli: &Cli, bundle: &RuntimeBundle) -> Result<SessionHandle, PrintError> {
    if cli.no_session {
        return Ok(SessionHandle {
            store: Arc::new(EventStore::new()),
            entry: None,
        });
    }

    let manager = SessionManager::new(session_data_dir());
    let opened = if let Some(id) = cli.resume.as_deref() {
        open_resume_request(&manager, id)?
    } else if let Some(id) = cli.fork.as_deref() {
        open_fork_request(&manager, id, bundle)?
    } else if let Some(id) = cli.session_id.as_deref() {
        let options = CreateSessionOptions {
            model: bundle.model.clone(),
            working_dir: working_dir_string()?,
            name: cli.session_name.clone(),
        };
        if cli.resume_if_exists {
            manager.open_or_resume(id, options, DurabilityPolicy::Flush)?
        } else {
            manager.create_with_id(id, options, DurabilityPolicy::Flush)?
        }
    } else {
        manager.create(
            CreateSessionOptions {
                model: bundle.model.clone(),
                working_dir: working_dir_string()?,
                name: cli.session_name.clone(),
            },
            DurabilityPolicy::Flush,
        )?
    };
    warn_if_lines_skipped(&opened);
    Ok(SessionHandle {
        store: Arc::new(opened.store),
        entry: Some(opened.entry),
    })
}

fn open_resume_request(
    manager: &SessionManager,
    id_or_name: &str,
) -> Result<OpenSession, PrintError> {
    if id_or_name.trim().is_empty() {
        let working_dir = working_dir_string()?;
        return Ok(manager.resume_latest_in_working_dir(&working_dir, DurabilityPolicy::Flush)?);
    }
    Ok(manager.resume(id_or_name, DurabilityPolicy::Flush)?)
}

fn open_fork_request(
    manager: &SessionManager,
    id_or_name: &str,
    bundle: &RuntimeBundle,
) -> Result<OpenSession, PrintError> {
    let working_dir = working_dir_string()?;
    let options = CreateSessionOptions {
        model: bundle.model.clone(),
        working_dir: working_dir.clone(),
        name: None,
    };
    if id_or_name.trim().is_empty() {
        return Ok(manager.fork_latest_in_working_dir(
            &working_dir,
            options,
            DurabilityPolicy::Flush,
        )?);
    }
    Ok(manager.fork(id_or_name, options, DurabilityPolicy::Flush)?)
}

/// Surface a partial replay on stderr: the tolerant reader skips torn,
/// corrupt, unknown, and duplicate lines instead of failing the load,
/// and that count must reach the user.
fn warn_if_lines_skipped(opened: &OpenSession) {
    if opened.replay.skipped_lines > 0 {
        eprintln!(
            "norn: warning: {} corrupt or unreadable line(s) skipped while loading session {}",
            opened.replay.skipped_lines, opened.entry.id,
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, unsafe_code)]
mod tests {
    use std::path::{Path, PathBuf};

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

    struct TempCurrentDir {
        prior: PathBuf,
        current: PathBuf,
    }

    impl TempCurrentDir {
        fn new(path: &Path) -> Self {
            let prior = std::env::current_dir().unwrap();
            std::env::set_current_dir(path).unwrap();
            Self {
                prior,
                current: path.to_owned(),
            }
        }

        fn set(&mut self, path: &Path) {
            self.current = path.to_owned();
            std::env::set_current_dir(&self.current).unwrap();
        }
    }

    impl Drop for TempCurrentDir {
        fn drop(&mut self) {
            if let Err(err) = std::env::set_current_dir(&self.prior) {
                eprintln!(
                    "failed to restore test working directory {}: {err}",
                    self.prior.display(),
                );
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

    /// `--session-id` creates the session under the caller's exact ID
    /// (resumable by it), and a second create with the same ID is a
    /// typed failure — never a silent attach to the first session.
    #[test]
    #[serial_test::serial]
    fn session_id_flag_creates_exactly_that_session_once() {
        let _guard = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = Cli::try_parse_from(["norn", "--session-id", "wf-run-42"]).unwrap();
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let session = open_session(&cli, &bundle).unwrap();
        assert_eq!(session.id(), Some("wf-run-42"));
        drop(session);

        let Err(err) = open_session(&cli, &bundle) else {
            panic!("an existing --session-id must fail loudly");
        };
        assert!(
            format!("{err:?}").contains("wf-run-42"),
            "the failure names the colliding id: {err:?}",
        );

        let resume_cli = Cli::try_parse_from(["norn", "--resume", "wf-run-42"]).unwrap();
        let resumed = open_session(&resume_cli, &bundle).unwrap();
        assert_eq!(
            resumed.id(),
            Some("wf-run-42"),
            "the caller-chosen id is resumable",
        );
    }

    /// `--session-id --resume-if-exists` is the idempotent counterpart
    /// to create-exactly-this: the first open creates, and later opens
    /// resume the same exact ID with prior events replayed.
    #[test]
    #[serial_test::serial]
    fn session_id_resume_if_exists_creates_then_resumes() {
        let _guard = TempNornHome::new(tempfile::tempdir().unwrap());
        let cli = Cli::try_parse_from(["norn", "--session-id", "wf-run-43", "--resume-if-exists"])
            .unwrap();
        let bundle = build_runtime(&cli, RuntimeInputs::default()).unwrap();
        let session = open_session(&cli, &bundle).unwrap();
        assert_eq!(session.id(), Some("wf-run-43"));
        session
            .store
            .append(SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "first attempt".to_owned(),
            })
            .unwrap();
        drop(session);

        let resumed = open_session(&cli, &bundle).unwrap();
        assert_eq!(resumed.id(), Some("wf-run-43"));
        assert_eq!(resumed.store.len(), 1, "prior history must be replayed");

        let index = read_index(&session_data_dir()).unwrap();
        assert_eq!(index.len(), 1, "retry must not create a duplicate entry");
        assert_eq!(index[0].id, "wf-run-43");
    }

    #[test]
    #[serial_test::serial]
    fn resume_without_id_selects_latest_session_for_current_working_dir() {
        let _home = TempNornHome::new(tempfile::tempdir().unwrap());
        let workspace = tempfile::tempdir().unwrap();
        let current_dir = workspace.path().join("current");
        let other_dir = workspace.path().join("other");
        std::fs::create_dir_all(&current_dir).unwrap();
        std::fs::create_dir_all(&other_dir).unwrap();
        let mut cwd = TempCurrentDir::new(&current_dir);

        let create_cli = Cli::try_parse_from(["norn"]).unwrap();
        let bundle = build_runtime(&create_cli, RuntimeInputs::default()).unwrap();
        let current_session = open_session(&create_cli, &bundle).unwrap();
        let current_id = current_session
            .id()
            .expect("created current-dir session")
            .to_owned();
        drop(current_session);

        std::thread::sleep(std::time::Duration::from_millis(5));
        cwd.set(&other_dir);
        let other_session = open_session(&create_cli, &bundle).unwrap();
        let other_id = other_session
            .id()
            .expect("created other-dir session")
            .to_owned();
        drop(other_session);

        cwd.set(&current_dir);
        let resume_cli = Cli::try_parse_from(["norn", "--resume"]).unwrap();
        let resumed = open_session(&resume_cli, &bundle).unwrap();
        assert_eq!(
            resumed.id(),
            Some(current_id.as_str()),
            "must not resume globally newer other-dir session {other_id}",
        );
    }

    /// clap rejects every contradictory pairing of the session-control
    /// flags — `--session-id` against all three, and the older trio
    /// against each other (previously silently prioritized by branch
    /// order in `open_session`).
    #[test]
    fn session_control_flags_conflict_pairwise() {
        for combo in [
            ["norn", "--session-id", "x", "--resume"].as_slice(),
            ["norn", "--session-id", "x", "--fork"].as_slice(),
            ["norn", "--session-id", "x", "--no-session"].as_slice(),
            ["norn", "--resume-if-exists"].as_slice(),
            ["norn", "--resume", "a", "--fork", "b"].as_slice(),
            ["norn", "--resume", "a", "--no-session"].as_slice(),
            ["norn", "--fork", "a", "--no-session"].as_slice(),
        ] {
            assert!(
                Cli::try_parse_from(combo.iter().copied()).is_err(),
                "expected a parse conflict for {combo:?}",
            );
        }
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
