//! Export formatting for `norn session export` (NC-008 R6).

use std::path::Path;

use crate::cli::ExitCode;
use crate::cli::SessionExportFormat;
use crate::session::{
    SessionIndexEntry, SessionPersistError, read_session_events, resolve_session,
};

use norn::session::events::SessionEvent;

/// Execute the `norn session export` subcommand: resolve the session,
/// load its events, and render in the requested format.
pub fn run_export(data_dir: &Path, input: &str, format: Option<SessionExportFormat>) -> ExitCode {
    let entry = match resolve_session(data_dir, input) {
        Ok(entry) => entry,
        Err(err) => return report_export_error(&err),
    };
    let events = match read_session_events(data_dir, &entry.id) {
        Ok(events) => events,
        Err(err) => return report_export_error(&err),
    };

    match format.unwrap_or(SessionExportFormat::Jsonl) {
        SessionExportFormat::Jsonl => export_jsonl(&events),
        SessionExportFormat::Json => export_json(&entry, &events),
        SessionExportFormat::Markdown => export_markdown(&entry, &events),
    }
}

fn report_export_error(err: &SessionPersistError) -> ExitCode {
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
        other => {
            eprintln!("norn: session persistence error: {other}");
        }
    }
    ExitCode::AgentError
}

fn export_jsonl(events: &[SessionEvent]) -> ExitCode {
    for event in events {
        match serde_json::to_string(event) {
            Ok(line) => println!("{line}"),
            Err(err) => {
                eprintln!("norn: failed to serialise event: {err}");
                return ExitCode::AgentError;
            }
        }
    }
    ExitCode::Success
}

fn export_json(entry: &SessionIndexEntry, events: &[SessionEvent]) -> ExitCode {
    let doc = serde_json::json!({
        "session": entry,
        "events": events,
    });
    match serde_json::to_string_pretty(&doc) {
        Ok(text) => {
            println!("{text}");
            ExitCode::Success
        }
        Err(err) => {
            eprintln!("norn: failed to serialise session document: {err}");
            ExitCode::AgentError
        }
    }
}

fn export_markdown(entry: &SessionIndexEntry, events: &[SessionEvent]) -> ExitCode {
    println!("# Session {}", entry.id);
    if let Some(name) = entry.name.as_deref() {
        println!("**Name:** {name}");
    }
    println!("**Model:** {}", entry.model);
    println!("**Working Dir:** {}", entry.working_dir);
    println!(
        "**Created:** {}",
        entry.created_at.format("%Y-%m-%dT%H:%M:%SZ")
    );
    println!(
        "**Updated:** {}",
        entry.updated_at.format("%Y-%m-%dT%H:%M:%SZ")
    );
    println!();

    for event in events {
        match event {
            SessionEvent::UserMessage { content, .. } => {
                println!("## User\n\n{content}\n");
            }
            SessionEvent::AssistantMessage {
                content,
                tool_calls,
                ..
            } => {
                println!("## Assistant\n");
                if !content.is_empty() {
                    println!("{content}\n");
                }
                for call in tool_calls {
                    println!("### Tool Call: {}\n", call.name);
                    println!("```json\n{}\n```\n", call.arguments);
                }
            }
            SessionEvent::SpokenResponse { content, .. } => {
                let rendered =
                    serde_json::to_string_pretty(content).unwrap_or_else(|_| content.to_string());
                println!("### Spoken Response\n\n```json\n{rendered}\n```\n");
            }
            SessionEvent::ToolResult {
                tool_name, output, ..
            } => {
                let rendered =
                    serde_json::to_string_pretty(output).unwrap_or_else(|_| output.to_string());
                println!("### Tool Result: {tool_name}\n\n```json\n{rendered}\n```\n");
            }
            SessionEvent::ModelChange {
                old_model,
                new_model,
                ..
            } => {
                println!("_Model changed: {old_model} -> {new_model}_\n");
            }
            SessionEvent::Compaction { summary, .. } => {
                println!("_Compaction: {summary}_\n");
            }
            SessionEvent::Fork {
                forked_session_id, ..
            } => {
                println!("_Fork -> {forked_session_id}_\n");
            }
            SessionEvent::ForkComplete {
                forked_session_id,
                duration_ms,
                ..
            } => {
                println!("_Fork complete <- {forked_session_id} ({duration_ms}ms)_\n");
            }
            SessionEvent::Label {
                label, description, ..
            } => match description {
                Some(desc) => println!("_Label `{label}`: {desc}_\n"),
                None => println!("_Label `{label}`_\n"),
            },
            SessionEvent::Custom {
                event_type, data, ..
            } => {
                let rendered = serde_json::to_string(data).unwrap_or_else(|_| data.to_string());
                println!("_Custom `{event_type}`: {rendered}_\n");
            }
        }
    }
    ExitCode::Success
}
