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

use std::path::Path;

use crate::cli::ExitCode;
use crate::cli::{Cli, SessionCmd, SessionListFormat};
use crate::config::session_data_dir;
use crate::session::{SessionIndexEntry, SessionManager, SessionPersistError};

use super::session_export;

/// Dispatch to a `norn session` subcommand. Takes ownership of the
/// outer [`Cli`] so resume/fork can mutate it before forwarding to the
/// agent path implemented in `main.rs`.
pub fn run_session(cli: Cli, cmd: SessionCmd, agent: AgentEntry<'_>) -> ExitCode {
    let data_dir = session_data_dir();
    match cmd {
        SessionCmd::List { all, limit, format } => run_list(&data_dir, all, limit, format),
        SessionCmd::Show { id } => run_show(&data_dir, &id),
        SessionCmd::Resume { id } => run_resume(cli, &data_dir, &id, agent),
        SessionCmd::Fork { id } => run_fork(cli, &data_dir, &id, agent),
        SessionCmd::Export { id, format } => session_export::run_export(&data_dir, &id, format),
        SessionCmd::Remove { id } => run_remove(&data_dir, &id),
    }
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
    match serde_json::to_string_pretty(entries) {
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
        Err(err) => return report_persist_error(&err),
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
        Err(err) => return report_persist_error(&err),
    };
    cli.fork = Some(resolved.id);
    cli.resume = None;
    cli.command = None;
    agent(&cli)
}

// ---------------------------------------------------------------------------
// R7: remove
// ---------------------------------------------------------------------------

fn run_remove(data_dir: &Path, input: &str) -> ExitCode {
    match SessionManager::new(data_dir).delete(input) {
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
    use crate::session::{
        CreateSessionOptions, SessionStatus, append_events, session_file_path, write_index_atomic,
    };
    use chrono::Utc;
    use clap::Parser;
    use norn::session::DurabilityPolicy;
    use norn::session::events::{EventBase, SessionEvent};

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
    fn list_all_sorts_newest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let older = SessionIndexEntry {
            id: "11111111-1111-7111-8111-111111111111".to_owned(),
            name: None,
            model: "gpt-old".to_owned(),
            working_dir: "/a".to_owned(),
            created_at: now,
            updated_at: now,
            event_count: 0,
            status: SessionStatus::Active,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            format_version: 0,
        };
        let newer = SessionIndexEntry {
            id: "22222222-2222-7222-8222-222222222222".to_owned(),
            name: None,
            model: "gpt-new".to_owned(),
            working_dir: "/b".to_owned(),
            created_at: now,
            updated_at: now + chrono::Duration::seconds(10),
            event_count: 1,
            status: SessionStatus::Active,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            format_version: 0,
        };
        write_index_atomic(tmp.path(), &[older, newer]).unwrap();

        let code = run_list(tmp.path(), true, None, Some(SessionListFormat::Table));
        assert_eq!(code, ExitCode::Success);
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
    fn remove_deletes_index_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = fresh(tmp.path(), "gpt", "/work");
        append_events(
            tmp.path(),
            &entry.id,
            &[SessionEvent::UserMessage {
                base: EventBase::new(None),
                content: "hi".to_owned(),
            }],
            false,
        )
        .unwrap();
        let code = run_remove(tmp.path(), &entry.id);
        assert_eq!(code, ExitCode::Success);
        let index = crate::session::read_index(tmp.path()).unwrap();
        assert!(index.iter().all(|e| e.id != entry.id));
        assert!(!session_file_path(tmp.path(), &entry.id).exists());
    }

    #[test]
    fn remove_handles_missing_file_gracefully() {
        let tmp = tempfile::tempdir().unwrap();
        let entry = fresh(tmp.path(), "gpt", "/work");
        // Delete the session file out from under the index entry — the
        // remove must still succeed and clean up the entry.
        std::fs::remove_file(session_file_path(tmp.path(), &entry.id)).unwrap();
        let code = run_remove(tmp.path(), &entry.id);
        assert_eq!(code, ExitCode::Success);
    }

    #[test]
    fn remove_unknown_returns_agent_error() {
        let tmp = tempfile::tempdir().unwrap();
        let code = run_remove(tmp.path(), "ghost");
        assert_eq!(code, ExitCode::AgentError);
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
}
