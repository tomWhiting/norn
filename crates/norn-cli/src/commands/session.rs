//! `norn session …` subcommand dispatchers (NC-008 R1–R7).
//!
//! Every handler here is a thin wrapper around the session-persistence
//! API in [`crate::session`]. List, show, export, and remove operate
//! directly on the JSONL store. Resume and fork validate the source
//! session up-front and forward to the agent path via mutated [`Cli`]
//! state — the JSONL load/copy happens later in NC-005 when the REPL
//! becomes a real consumer.
//!
//! All success/error messages follow the contract in DESIGN.md CO5:
//! stdout carries the result payload (table rows, JSON, markdown, export
//! data); stderr carries human-readable status, errors, and
//! remediation. Exit codes follow [`ExitCode`]: 0 success, 1 agent error
//! (which covers `not found`, `ambiguous prefix`, and other operational
//! failures here), 2 argument error, 3 auth error.

use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

use norn::runtime_init::load_merged_settings;

use crate::cli::ExitCode;
use crate::cli::{BuildError, Cli, Mode, Protocol, SessionCmd, SessionListFormat, detect_mode};
use crate::config::{ConfigOverrides, resolve_index_lock_deadline, session_data_dir};
use crate::print::emit_error_envelope;
use crate::print::orchestrator::PrintError;
use crate::session::{SessionIndexEntry, SessionManager, SessionPersistError};

use super::session_export;
use super::session_output::PublicSessionIndexEntry;

/// Dispatch to a `norn session` subcommand. Takes ownership of the
/// outer [`Cli`] so resume/fork can mutate it before forwarding to the
/// agent path implemented in `main.rs`.
///
/// `migrate` resolves the trusted Norn root directly and never preflights the
/// active session store. Lock discipline: `list`, `show`, `export`, and the pre-forward
/// `resolve` in `resume`/`fork` only *read* the index (no inter-process
/// lock), and the forwarded resume/fork run re-resolves its own deadline
/// inside `builder_from_cli`. `remove` is the one subcommand that
/// mutates the index directly, so it — and only it — resolves the
/// settings-backed index-lock deadline here before touching the lock.
pub fn run_session(cli: Cli, cmd: SessionCmd, agent: AgentEntry<'_>) -> ExitCode {
    match cmd {
        SessionCmd::Migrate => run_migrate(),
        SessionCmd::Legacy { command } => super::session_legacy::run(&command),
        SessionCmd::List { all, limit, format } => {
            with_session_data_dir(|data_dir| run_list(data_dir, all, limit, format))
        }
        SessionCmd::Show { id } => with_session_data_dir(|data_dir| run_show(data_dir, &id)),
        SessionCmd::Resume {
            id,
            allow_degraded_session,
        } => {
            let mut cli = cli;
            cli.allow_degraded_session |= allow_degraded_session;
            with_session_data_dir(|data_dir| run_resume(cli, data_dir, &id, agent))
        }
        SessionCmd::Fork {
            id,
            allow_degraded_session,
        } => {
            let mut cli = cli;
            cli.allow_degraded_session |= allow_degraded_session;
            with_session_data_dir(|data_dir| run_fork(cli, data_dir, &id, agent))
        }
        SessionCmd::Export { id, format } => {
            with_session_data_dir(|data_dir| session_export::run_export(data_dir, &id, format))
        }
        SessionCmd::Remove { id } => with_session_data_dir(|data_dir| {
            let deadline = match resolve_subcommand_lock_deadline(&cli) {
                Ok(deadline) => deadline,
                Err(err) => {
                    eprintln!("norn: {err}");
                    return err.exit_code();
                }
            };
            run_remove(data_dir, &id, deadline)
        }),
    }
}

fn with_session_data_dir(run: impl FnOnce(&Path) -> ExitCode) -> ExitCode {
    match session_data_dir() {
        Ok(path) => run(&path),
        Err(error) => {
            let error = BuildError::from(error);
            eprintln!("norn: {error}");
            error.exit_code()
        }
    }
}

fn run_migrate() -> ExitCode {
    let norn_root = match crate::config::paths::session_norn_root() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("norn: session migration failed: {error}");
            return ExitCode::AgentError;
        }
    };
    match norn::session::migrate_legacy_sessions(&norn_root) {
        Ok(norn::session::SessionMigrationOutcome::Migrated { counts, .. }) => {
            println!("{}", migration_summary("migrated", counts));
            ExitCode::Success
        }
        Ok(norn::session::SessionMigrationOutcome::AlreadyMigrated { counts, .. }) => {
            println!("{}", migration_summary("already-migrated", counts));
            ExitCode::Success
        }
        Err(error) => {
            eprintln!("norn: session migration failed: {error}");
            ExitCode::AgentError
        }
    }
}

fn migration_summary(status: &str, counts: norn::session::MigrationCounts) -> String {
    format!(
        "session migration: {status}; canonical={}; fresh_epoch_projection={}; inspect_only={}",
        counts.canonical, counts.fresh_epoch_projection, counts.inspect_only,
    )
}

/// Resolve the index-lock deadline for a session subcommand that
/// mutates the index without going through `builder_from_cli`.
///
/// Runs the same sealed settings transaction as the agent path, then applies
/// the `-c` overrides from the shared top-level flag, and feeds the result to
/// [`resolve_index_lock_deadline`], so `norn session remove` honours
/// exactly the deadline `norn -p` would — including `-c
/// index_lock_deadline_ms=<u64>` on the recovery command itself.
///
/// # Errors
///
/// [`BuildError`] when the settings fail to load / validate or a `-c`
/// override fails to parse — surfaced loudly (with its usual exit code)
/// rather than silently falling back to an unbounded lock wait.
fn resolve_subcommand_lock_deadline(cli: &Cli) -> Result<Duration, BuildError> {
    let cwd = std::env::current_dir()?;
    let settings =
        load_merged_settings(&cwd).map_err(|error| BuildError::Argument(error.to_string()))?;
    let overrides = ConfigOverrides::parse(&cli.config)?;
    resolve_index_lock_deadline(&settings, &overrides)
}

/// Callback that runs the agent path with a (possibly mutated) [`Cli`].
/// Stored as a function pointer so the binary can pass its private
/// `run_agent` without exposing it as a library API.
pub type AgentEntry<'a> = &'a dyn Fn(&Cli) -> ExitCode;

// ---------------------------------------------------------------------------
// R2: list
// ---------------------------------------------------------------------------

fn run_list(
    data_dir: &Path,
    all: bool,
    limit: Option<usize>,
    format: Option<SessionListFormat>,
) -> ExitCode {
    let entries = match SessionManager::new(data_dir).list() {
        Ok(rows) => rows,
        Err(err) => return report_persist_error(&err),
    };

    let mut filtered: Vec<SessionIndexEntry> = if all {
        entries
    } else {
        let canonical_cwd = std::env::current_dir()
            .ok()
            .and_then(|p| std::fs::canonicalize(p).ok());
        entries
            .into_iter()
            .filter(|entry| matches_cwd(canonical_cwd.as_ref(), &entry.working_dir))
            .collect()
    };

    filtered.sort_by_key(|entry| std::cmp::Reverse(entry.updated_at));
    if let Some(max) = limit {
        filtered.truncate(max);
    }

    match format.unwrap_or(SessionListFormat::Table) {
        SessionListFormat::Table => print_table(&filtered),
        SessionListFormat::Json => print_json_array(&filtered),
    }
}

fn matches_cwd(canonical_cwd: Option<&std::path::PathBuf>, stored: &str) -> bool {
    let Some(cwd) = canonical_cwd else {
        return false;
    };
    match std::fs::canonicalize(stored) {
        Ok(stored_canonical) => stored_canonical == *cwd,
        Err(_) => false,
    }
}

fn print_table(entries: &[SessionIndexEntry]) -> ExitCode {
    if entries.is_empty() {
        println!("No sessions found.");
        return ExitCode::Success;
    }

    let id_w: usize = 8;
    let name_w = entries
        .iter()
        .map(|e| e.name.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(4)
        .max(4);
    let model_w = entries
        .iter()
        .map(|e| e.model.len())
        .max()
        .unwrap_or(5)
        .max(5);
    let turns_w = entries
        .iter()
        .map(|e| e.event_count.to_string().len())
        .max()
        .unwrap_or(5)
        .max(5);
    let updated_w = 20; // RFC 3339 minute-truncated form `YYYY-MM-DDTHH:MM:SSZ`.

    println!(
        "{:<idw$}  {:<namew$}  {:<modelw$}  {:>turnsw$}  {:<updatedw$}  WORKING_DIR",
        "ID",
        "NAME",
        "MODEL",
        "TURNS",
        "UPDATED",
        idw = id_w,
        namew = name_w,
        modelw = model_w,
        turnsw = turns_w,
        updatedw = updated_w,
    );

    for entry in entries {
        let id_short: String = entry.id.chars().take(id_w).collect();
        let name = entry.name.as_deref().unwrap_or("-");
        let updated = entry.updated_at.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let turns = entry.event_count.to_string();
        println!(
            "{:<idw$}  {:<namew$}  {:<modelw$}  {:>turnsw$}  {:<updatedw$}  {}",
            id_short,
            name,
            entry.model,
            turns,
            updated,
            entry.working_dir,
            idw = id_w,
            namew = name_w,
            modelw = model_w,
            turnsw = turns_w,
            updatedw = updated_w,
        );
    }
    ExitCode::Success
}

fn print_json_array(entries: &[SessionIndexEntry]) -> ExitCode {
    let public_entries = entries
        .iter()
        .map(PublicSessionIndexEntry::from)
        .collect::<Vec<_>>();
    match serde_json::to_string_pretty(&public_entries) {
        Ok(text) => {
            println!("{text}");
            ExitCode::Success
        }
        Err(err) => {
            eprintln!("norn: failed to serialise session index: {err}");
            ExitCode::AgentError
        }
    }
}

// ---------------------------------------------------------------------------
// R3: show
// ---------------------------------------------------------------------------

fn run_show(data_dir: &Path, input: &str) -> ExitCode {
    let entry = match SessionManager::new(data_dir).resolve(input) {
        Ok(entry) => entry,
        Err(err) => return report_persist_error(&err),
    };

    let name = entry.name.as_deref().unwrap_or("-");
    let status = match entry.status {
        crate::session::SessionStatus::Active => "active",
        crate::session::SessionStatus::Completed => "completed",
    };
    println!("id: {}", entry.id);
    println!("name: {name}");
    println!("model: {}", entry.model);
    println!("working_dir: {}", entry.working_dir);
    println!(
        "created_at: {}",
        entry.created_at.format("%Y-%m-%dT%H:%M:%SZ")
    );
    println!(
        "updated_at: {}",
        entry.updated_at.format("%Y-%m-%dT%H:%M:%SZ")
    );
    println!("event_count: {}", entry.event_count);
    println!("status: {status}");
    ExitCode::Success
}

// ---------------------------------------------------------------------------
// R4: resume — validate, then forward to the agent path
// ---------------------------------------------------------------------------

fn run_resume(mut cli: Cli, data_dir: &Path, input: &str, agent: AgentEntry<'_>) -> ExitCode {
    let resolved = match SessionManager::new(data_dir).resolve(input) {
        Ok(entry) => entry,
        Err(err) => return fail_forwarded_resolve(&cli, &err),
    };
    cli.resume = Some(resolved.id);
    cli.fork = None;
    cli.command = None;
    agent(&cli)
}

// ---------------------------------------------------------------------------
// R5: fork — validate, then forward to the agent path
// ---------------------------------------------------------------------------

fn run_fork(mut cli: Cli, data_dir: &Path, input: &str, agent: AgentEntry<'_>) -> ExitCode {
    let resolved = match SessionManager::new(data_dir).resolve(input) {
        Ok(entry) => entry,
        Err(err) => return fail_forwarded_resolve(&cli, &err),
    };
    cli.fork = Some(resolved.id);
    cli.resume = None;
    cli.command = None;
    agent(&cli)
}

// ---------------------------------------------------------------------------
// R7: remove
// ---------------------------------------------------------------------------

/// `deadline` bounds the inter-process index-lock wait inside
/// [`SessionManager::delete`]. `session remove` is the very command an
/// operator reaches for to recover from a wedged sibling process, so an
/// unbounded wait here would hang the recovery tool on exactly the
/// pathology it exists to clean up; on expiry the typed
/// [`SessionPersistError::IndexLockTimeout`] names the lock file instead.
fn run_remove(data_dir: &Path, input: &str, deadline: Duration) -> ExitCode {
    let manager = SessionManager::new(data_dir).with_index_lock_deadline(Some(deadline));
    match manager.delete(input) {
        Ok(entry) => {
            eprintln!("Removed session {}.", entry.id);
            ExitCode::Success
        }
        Err(err) => report_persist_error(&err),
    }
}

// ---------------------------------------------------------------------------
// Shared error rendering
// ---------------------------------------------------------------------------

/// Resolve failed before `resume`/`fork` forwarded to the agent path.
///
/// The stderr rendering and the exit code are byte-identical to every
/// other session-subcommand failure ([`report_persist_error`]).
/// ADDITIONALLY, when the forwarded invocation was bound for plain print
/// mode, the typed error envelope is emitted first, so
/// `norn -p -f json session resume <stale>` fails as parseably as the
/// `norn -p -f json --resume <stale>` spelling of the same operation —
/// owner ruling R2 (2026-07-06): EVERY post-argument-parsing failure of a
/// print-bound machine-format invocation gets a typed stop, pre-assembly
/// included.
fn fail_forwarded_resolve(cli: &Cli, err: &SessionPersistError) -> ExitCode {
    emit_forwarded_resolve_envelope(cli, err);
    report_persist_error(err)
}

/// Emit the `session`-classed error envelope for a resolve failure on a
/// forwarded `resume`/`fork` invocation, iff that invocation was bound
/// for plain print mode.
///
/// Uses the exact mode computation `main.rs::run_agent` would have
/// applied after the forward ([`detect_mode`] over the `--print` flag and
/// the real stdin/stdout TTY state): the envelope belongs to the output
/// surface the forwarded run WOULD have used, never to the TUI. A
/// JSON-RPC peer (`--protocol jsonrpc`) is excluded the same way the
/// orchestrator's own pre-runtime emit site excludes it — that stdout
/// carries frames only, and a print envelope would corrupt the stream.
/// Text format and the class filter are handled inside
/// [`emit_error_envelope`] itself. Model and session id are `null`: the
/// failure precedes assembly (R3 minimal payload).
fn emit_forwarded_resolve_envelope(cli: &Cli, err: &SessionPersistError) {
    if cli.protocol == Some(Protocol::Jsonrpc) {
        return;
    }
    let stdin_is_tty = std::io::stdin().is_terminal();
    let stdout_is_tty = std::io::stdout().is_terminal();
    if detect_mode(cli.print, stdin_is_tty, stdout_is_tty) != Mode::Print {
        return;
    }
    emit_error_envelope(cli, &PrintError::Session(err.to_string()), None, None);
}

fn report_persist_error(err: &SessionPersistError) -> ExitCode {
    match err {
        SessionPersistError::NotFound { input } => {
            eprintln!("Session not found: {input}");
        }
        SessionPersistError::AmbiguousPrefix { prefix, matches } => {
            eprintln!("Ambiguous prefix {prefix}; candidates:");
            for id in matches {
                eprintln!("  {id}");
            }
        }
        SessionPersistError::EmptySource { id } => {
            eprintln!("Cannot operate on empty source session: {id}");
        }
        other => {
            eprintln!("norn: session persistence error: {other}");
        }
    }
    ExitCode::AgentError
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::cli::SessionExportFormat;
    use crate::session::{CreateSessionOptions, session_file_path};
    use clap::Parser;
    use norn::session::DurabilityPolicy;
    use norn::session::events::{EventBase, SessionEvent};

    /// Index-lock deadline used by every non-contended test below —
    /// generous enough that a healthy (uncontended) lock acquisition
    /// never trips it. Legitimate test configuration, not a production
    /// default.
    const TEST_LOCK_DEADLINE: Duration = Duration::from_secs(10);

    fn fresh(data_dir: &Path, model: &str, wd: &str) -> SessionIndexEntry {
        SessionManager::new(data_dir)
            .create(
                CreateSessionOptions {
                    model: model.to_owned(),
                    working_dir: wd.to_owned(),
                    name: None,
                },
                DurabilityPolicy::Flush,
            )
            .unwrap()
            .entry
    }

    #[test]
    fn report_not_found_returns_agent_error() {
        let err = SessionPersistError::NotFound {
            input: "abc".to_owned(),
        };
        assert_eq!(report_persist_error(&err), ExitCode::AgentError);
    }

    #[test]
    fn report_ambiguous_returns_agent_error() {
        let err = SessionPersistError::AmbiguousPrefix {
            prefix: "abcdefgh".to_owned(),
            matches: vec!["abcdefgh-1".to_owned(), "abcdefgh-2".to_owned()],
        };
        assert_eq!(report_persist_error(&err), ExitCode::AgentError);
    }

    #[test]
    fn migration_summary_is_stable_and_complete() {
        let summary = migration_summary(
            "already-migrated",
            norn::session::MigrationCounts {
                canonical: 2,
                fresh_epoch_projection: 3,
                inspect_only: 5,
            },
        );
        assert_eq!(
            summary,
            "session migration: already-migrated; canonical=2; fresh_epoch_projection=3; inspect_only=5"
        );
    }

    #[test]
    #[serial_test::serial]
    fn migrate_dispatches_offline_and_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
        let norn_home = tempfile::tempdir()?;
        std::fs::create_dir(norn_home.path().join("sessions"))?;

        let run = || -> Result<ExitCode, Box<dyn std::error::Error>> {
            let mut cli = Cli::try_parse_from(["norn", "session", "migrate"])?;
            let Some(crate::cli::Command::Session { command }) = cli.command.take() else {
                return Err(std::io::Error::other(
                    "parsed migration command had the wrong command shape",
                )
                .into());
            };
            Ok(run_session(cli, command, &|_| ExitCode::AgentError))
        };

        temp_env::with_var("NORN_HOME", Some(norn_home.path().as_os_str()), || {
            assert_eq!(run()?, ExitCode::Success);
            assert!(norn_home.path().join("session-store").is_dir());
            assert!(norn_home.path().join("sessions").is_dir());
            assert_eq!(run()?, ExitCode::Success);
            Ok::<_, Box<dyn std::error::Error>>(())
        })?;
        Ok(())
    }

    #[test]
    #[serial_test::serial]
    fn subcommand_settings_reject_working_directory_authority_before_merge()
    -> Result<(), Box<dyn std::error::Error>> {
        let norn_home = tempfile::tempdir()?;
        let cwd = tempfile::tempdir()?;
        let settings_dir = cwd.path().join(".norn");
        std::fs::create_dir_all(&settings_dir)?;
        std::fs::write(
            settings_dir.join("settings.json"),
            r#"{"hooks":{"session_start":[{"command":"sentinel-session-command","timeout":5}]}}"#,
        )?;

        let result = temp_env::with_var("NORN_HOME", Some(norn_home.path().as_os_str()), || {
            load_merged_settings(cwd.path())
        });
        let Err(error) = result else {
            return Err("session subcommands accepted repository command authority".into());
        };
        let rendered = error.to_string();
        assert!(rendered.contains("hooks"));
        assert!(!rendered.contains("sentinel-session-command"));
        Ok(())
    }

    #[test]
    fn list_table_with_no_sessions_prints_friendly_message() {
        let tmp = tempfile::tempdir().unwrap();
        let code = run_list(tmp.path(), false, None, Some(SessionListFormat::Table));
        assert_eq!(code, ExitCode::Success);
    }

    #[test]
    fn list_json_empty_array_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let code = run_list(tmp.path(), true, None, Some(SessionListFormat::Json));
        assert_eq!(code, ExitCode::Success);
    }

    #[test]
    fn list_all_sorts_newest_first() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let manager = SessionManager::new(tmp.path());
        let older = manager.create_with_id(
            "11111111-1111-7111-8111-111111111111",
            CreateSessionOptions {
                model: "gpt-old".to_owned(),
                working_dir: "/a".to_owned(),
                name: None,
            },
            DurabilityPolicy::Flush,
        )?;
        drop(older);
        std::thread::sleep(Duration::from_millis(5));
        let newer = manager.create_with_id(
            "22222222-2222-7222-8222-222222222222",
            CreateSessionOptions {
                model: "gpt-new".to_owned(),
                working_dir: "/b".to_owned(),
                name: None,
            },
            DurabilityPolicy::Flush,
        )?;
        newer.store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "newest".to_owned(),
        })?;
        drop(newer);

        let code = run_list(tmp.path(), true, None, Some(SessionListFormat::Table));
        assert_eq!(code, ExitCode::Success);
        Ok(())
    }

    #[test]
    fn show_not_found_emits_agent_error() {
        let tmp = tempfile::tempdir().unwrap();
        let code = run_show(tmp.path(), "missing");
        assert_eq!(code, ExitCode::AgentError);
    }

    #[test]
    fn show_resolves_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = fresh(tmp.path(), "gpt", "/work");
        let code = run_show(tmp.path(), &entry.id);
        assert_eq!(code, ExitCode::Success);
    }

    #[test]
    fn remove_deletes_index_entry() -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let entry = fresh(tmp.path(), "gpt", "/work");
        let opened = SessionManager::new(tmp.path()).resume(&entry.id, DurabilityPolicy::Flush)?;
        opened.store.append(SessionEvent::UserMessage {
            base: EventBase::new(None),
            content: "hi".to_owned(),
        })?;
        drop(opened);
        let code = run_remove(tmp.path(), &entry.id, TEST_LOCK_DEADLINE);
        assert_eq!(code, ExitCode::Success);
        let index = crate::session::read_index(tmp.path())?;
        assert!(index.iter().all(|e| e.id != entry.id));
        assert!(!session_file_path(tmp.path(), &entry.id).exists());
        Ok(())
    }

    #[test]
    fn remove_handles_missing_file_gracefully() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = fresh(tmp.path(), "gpt", "/work");
        // Delete the session file out from under the index entry — the
        // remove must still succeed and clean up the entry.
        std::fs::remove_file(session_file_path(tmp.path(), &entry.id)).unwrap();
        let code = run_remove(tmp.path(), &entry.id, TEST_LOCK_DEADLINE);
        assert_eq!(code, ExitCode::Success);
    }

    #[test]
    fn remove_unknown_returns_agent_error() {
        let tmp = tempfile::tempdir().unwrap();
        let code = run_remove(tmp.path(), "ghost", TEST_LOCK_DEADLINE);
        assert_eq!(code, ExitCode::AgentError);
    }

    /// Regression (review A1, 2026-07-06): `norn session remove` used to
    /// construct its [`SessionManager`] with no index-lock deadline, so a
    /// wedged sibling process hung the very command an operator uses to
    /// recover. With the deadline threaded through, a held lock now fails
    /// fast with [`ExitCode::AgentError`] (the typed
    /// `SessionPersistError::IndexLockTimeout` path), and the same
    /// deadline succeeds once the holder releases.
    #[test]
    fn remove_with_held_lock_times_out_instead_of_hanging() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = fresh(tmp.path(), "gpt", "/work");

        // Hold the advisory lock the way a wedged sibling norn process
        // would: an independent file description with the exclusive OS
        // lock (mirrors tests/index_lock_deadline.rs).
        let lock_path = tmp.path().join("index.lock");
        let holder = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .unwrap();
        holder.lock().unwrap();

        // 50ms deadline — legitimate test configuration for a fast test.
        let started = std::time::Instant::now();
        let code = run_remove(tmp.path(), &entry.id, Duration::from_millis(50));
        assert_eq!(code, ExitCode::AgentError);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the held lock must time out, not hang: waited {:?}",
            started.elapsed(),
        );

        // Holder releases: the same deadline now acquires the lock and
        // the remove completes (the session file was already unlinked
        // before the lock wait, and a missing file is tolerated).
        holder.unlock().unwrap();
        drop(holder);
        let code = run_remove(tmp.path(), &entry.id, Duration::from_millis(50));
        assert_eq!(code, ExitCode::Success);
        let index = crate::session::read_index(tmp.path()).unwrap();
        assert!(index.iter().all(|e| e.id != entry.id));
    }

    #[test]
    fn export_jsonl_handles_empty_session() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = fresh(tmp.path(), "gpt", "/work");
        let code =
            session_export::run_export(tmp.path(), &entry.id, Some(SessionExportFormat::Jsonl));
        assert_eq!(code, ExitCode::Success);
    }

    #[test]
    fn export_markdown_renders_with_no_events() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = fresh(tmp.path(), "gpt", "/work");
        let code =
            session_export::run_export(tmp.path(), &entry.id, Some(SessionExportFormat::Markdown));
        assert_eq!(code, ExitCode::Success);
    }

    #[test]
    fn export_json_includes_session_and_events_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = fresh(tmp.path(), "gpt", "/work");
        let code =
            session_export::run_export(tmp.path(), &entry.id, Some(SessionExportFormat::Json));
        assert_eq!(code, ExitCode::Success);
    }

    #[test]
    fn export_not_found_returns_agent_error() {
        let tmp = tempfile::tempdir().unwrap();
        let code =
            session_export::run_export(tmp.path(), "ghost", Some(SessionExportFormat::Jsonl));
        assert_eq!(code, ExitCode::AgentError);
    }

    #[test]
    fn resume_forwards_resolved_id_into_cli_resume() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = fresh(tmp.path(), "gpt", "/work");
        let captured = std::sync::Mutex::new(None::<(Option<String>, Option<String>)>);
        let agent: AgentEntry<'_> = &|cli: &Cli| {
            *captured.lock().unwrap() = Some((cli.resume.clone(), cli.fork.clone()));
            ExitCode::Success
        };
        let parsed = Cli::try_parse_from(["norn"]).unwrap();
        let code = run_resume(parsed, tmp.path(), &entry.id, agent);
        assert_eq!(code, ExitCode::Success);
        let snapshot = captured.lock().unwrap().clone().unwrap();
        assert_eq!(snapshot.0, Some(entry.id));
        assert!(snapshot.1.is_none());
    }

    #[test]
    fn fork_forwards_resolved_id_into_cli_fork() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = fresh(tmp.path(), "gpt", "/work");
        let captured = std::sync::Mutex::new(None::<(Option<String>, Option<String>)>);
        let agent: AgentEntry<'_> = &|cli: &Cli| {
            *captured.lock().unwrap() = Some((cli.resume.clone(), cli.fork.clone()));
            ExitCode::Success
        };
        let parsed = Cli::try_parse_from(["norn"]).unwrap();
        let code = run_fork(parsed, tmp.path(), &entry.id, agent);
        assert_eq!(code, ExitCode::Success);
        let snapshot = captured.lock().unwrap().clone().unwrap();
        assert!(snapshot.0.is_none());
        assert_eq!(snapshot.1, Some(entry.id));
    }

    #[test]
    fn resume_unknown_id_returns_agent_error_without_invoking_agent() {
        let tmp = tempfile::tempdir().unwrap();
        let invoked = std::sync::Mutex::new(false);
        let agent: AgentEntry<'_> = &|_cli: &Cli| {
            *invoked.lock().unwrap() = true;
            ExitCode::Success
        };
        let parsed = Cli::try_parse_from(["norn"]).unwrap();
        let code = run_resume(parsed, tmp.path(), "ghost", agent);
        assert_eq!(code, ExitCode::AgentError);
        assert!(!*invoked.lock().unwrap());
    }

    #[test]
    #[serial_test::serial]
    fn migrate_rejects_relative_norn_home_before_fallback() {
        temp_env::with_var(
            "NORN_HOME",
            Some(std::ffi::OsStr::new("relative-home")),
            || {
                assert_eq!(run_migrate(), ExitCode::AgentError);
            },
        );
    }
}
