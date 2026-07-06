//! Tests for the session persistence layer (NC-002 R2--R6).

use std::fs;
use std::io::Write as _;
use std::path::Path;

use crate::provider::usage::Usage;
use crate::session::events::{
    ChildBranchKind, EventBase, EventId, EventUsage, SessionEvent, ToolCallEvent,
};
use crate::session::manager::{CreateSessionOptions, SessionManager};
use crate::session::store::{DurabilityPolicy, JsonlSink, PersistenceSink};
use chrono::Utc;

use super::*;

fn assistant_usage(input: u64, output: u64, cache_read: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        ..Usage::default()
    }
}

fn assistant_with_usage(input: u64, output: u64, cache_read: u64) -> SessionEvent {
    SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        content: "ok".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_write_tokens: 0,
            cost_usd: None,
        },
        stop_reason: "stop".to_owned(),
        response_id: None,
    }
}

fn user_msg(text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: text.to_owned(),
    }
}

fn one_of_each() -> Vec<SessionEvent> {
    let parent = EventId::new();
    vec![
        user_msg("hello"),
        SessionEvent::AssistantMessage {
            base: EventBase::new(None),
            content: "hi".to_owned(),
            thinking: String::new(),
            reasoning: Vec::new(),
            tool_calls: vec![ToolCallEvent {
                call_id: "call_tc1".to_owned(),
                name: "Read".to_owned(),
                arguments: serde_json::json!({"path": "/etc/hosts"}),
                kind: crate::provider::request::ToolCallKind::Function,
            }],
            usage: EventUsage::default(),
            stop_reason: String::new(),
            response_id: None,
        },
        SessionEvent::SpokenResponse {
            base: EventBase::new(None),
            content: serde_json::json!({"text": "spoken"}),
        },
        SessionEvent::ToolResult {
            base: EventBase::new(None),
            tool_call_id: "tc_1".to_owned(),
            tool_name: "Read".to_owned(),
            output: serde_json::json!({"bytes": 42}),
            duration_ms: 5,
        },
        SessionEvent::ModelChange {
            base: EventBase::new(None),
            old_model: "a".to_owned(),
            new_model: "b".to_owned(),
        },
        SessionEvent::Compaction {
            base: EventBase::new(None),
            summary: "summary".to_owned(),
            replaced_event_ids: vec![parent.clone()],
        },
        SessionEvent::ChildBranch {
            base: EventBase::new(Some(parent.clone())),
            parent_session_id: Some("parent-session".to_owned()),
            child_session_id: Some("child".to_owned()),
            path_address: "root/fork-1a2b3c4d".to_owned(),
            parent_event_anchor: Some(parent),
            kind: ChildBranchKind::Fork,
        },
        SessionEvent::Label {
            base: EventBase::new(None),
            label: "before-refactor".to_owned(),
            description: None,
        },
        SessionEvent::Custom {
            base: EventBase::new(None),
            event_type: "marker".to_owned(),
            data: serde_json::json!({"k": "v"}),
        },
    ]
}

fn assert_event_eq(a: &SessionEvent, b: &SessionEvent) {
    let ja = serde_json::to_string(a).unwrap();
    let jb = serde_json::to_string(b).unwrap();
    assert_eq!(ja, jb);
}

fn manager(dir: &Path) -> SessionManager {
    SessionManager::new(dir)
}

fn options(model: &str, working_dir: &str, name: Option<&str>) -> CreateSessionOptions {
    CreateSessionOptions {
        model: model.to_owned(),
        working_dir: working_dir.to_owned(),
        name: name.map(str::to_owned),
    }
}

/// Create a session through the manager and drop its store immediately,
/// leaving the index entry (and the header-only session file) behind —
/// the setup most batch-path tests want.
fn fresh_session(dir: &Path) -> SessionIndexEntry {
    fresh_session_at(dir, "/work")
}

fn fresh_session_at(dir: &Path, working_dir: &str) -> SessionIndexEntry {
    manager(dir)
        .create(options("gpt-x", working_dir, None), DurabilityPolicy::Flush)
        .unwrap()
        .entry
}

/// Register an index entry WITHOUT touching the session file at all —
/// for tests that assert the file's absence or that a lower layer
/// creates it.
fn index_only_entry(dir: &Path) -> SessionIndexEntry {
    let now = Utc::now();
    let entry = SessionIndexEntry {
        id: uuid::Uuid::now_v7().to_string(),
        name: None,
        model: "gpt-x".to_owned(),
        working_dir: "/work".to_owned(),
        created_at: now,
        updated_at: now,
        event_count: 0,
        status: SessionStatus::Active,
        format_version: SESSION_FORMAT_VERSION,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        rel_path: None,
        parent_id: None,
    };
    append_index_entry(dir, &entry, None).unwrap();
    entry
}

// ----- R2: JSONL serialization -----

#[test]
fn round_trip_all_nine_variants() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let events = one_of_each();
    append_events(tmp.path(), &entry.id, &events, false).unwrap();

    let read = read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(read.skipped_lines, 0);
    assert_eq!(read.events.len(), events.len());
    for (a, b) in events.iter().zip(read.events.iter()) {
        assert_event_eq(a, b);
    }
}

/// An `AssistantMessage` carrying reasoning items (encrypted and plain) must
/// survive a disk write → read-back, and the rebuilt provider messages must
/// carry the reasoning identical to what was persisted. This is the exact
/// resume path that a live conversation depends on to not shed reasoning
/// tokens on reload.
#[test]
fn round_trip_reasoning_items_through_disk() {
    use crate::provider::reasoning::{ReasoningItem, ReasoningSummaryPart};
    use crate::session::conversion::events_to_messages;

    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());

    let encrypted = ReasoningItem {
        id: "rs_enc".to_owned(),
        summary: vec![ReasoningSummaryPart::SummaryText {
            text: "encrypted thought".to_owned(),
        }],
        content: None,
        encrypted_content: Some("opaque-blob".to_owned()),
    };
    let plain = ReasoningItem {
        id: "rs_plain".to_owned(),
        summary: Vec::new(),
        content: None,
        encrypted_content: None,
    };
    let events = vec![SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        content: "answer".to_owned(),
        thinking: "summary".to_owned(),
        reasoning: vec![encrypted.clone(), plain.clone()],
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: None,
    }];
    append_events(tmp.path(), &entry.id, &events, false).unwrap();

    let read = read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(read.skipped_lines, 0);
    assert_eq!(read.events.len(), 1);
    assert_event_eq(&events[0], &read.events[0]);

    let msgs = events_to_messages(&read.events);
    assert_eq!(msgs.len(), 1);
    assert_eq!(
        msgs[0].reasoning,
        vec![encrypted, plain],
        "reasoning must round-trip through disk into the rebuilt message",
    );
}

#[test]
fn round_trip_at_least_five_variants() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let events: Vec<_> = one_of_each().into_iter().take(5).collect();
    append_events(tmp.path(), &entry.id, &events, false).unwrap();
    let read = read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(read.events.len(), 5);
    for (a, b) in events.iter().zip(read.events.iter()) {
        assert_event_eq(a, b);
    }
}

#[test]
fn each_jsonl_line_ends_with_newline() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &one_of_each(), false).unwrap();
    let content = fs::read_to_string(session_file_path(tmp.path(), &entry.id)).unwrap();
    assert!(content.ends_with('\n'));
    // First line is the version header, then one line per event.
    let line_count = content.lines().count();
    assert_eq!(line_count, one_of_each().len() + 1);
}

// ----- R4 (review): version header + tolerant reading -----

#[test]
fn header_written_at_creation_and_surfaced_on_read() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &[user_msg("hi")], false).unwrap();

    let body = fs::read_to_string(session_file_path(tmp.path(), &entry.id)).unwrap();
    let first = body.lines().next().unwrap();
    let header: SessionFileHeader = serde_json::from_str(first).unwrap();
    assert_eq!(header.version, SESSION_FORMAT_VERSION);

    let read = read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(read.format_version, Some(SESSION_FORMAT_VERSION));
    assert_eq!(read.events.len(), 1, "header must not be read as an event");
    assert_eq!(read.skipped_lines, 0);
}

#[test]
fn header_written_once_across_batches() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &[user_msg("a")], false).unwrap();
    append_events(tmp.path(), &entry.id, &[user_msg("b")], false).unwrap();
    let body = fs::read_to_string(session_file_path(tmp.path(), &entry.id)).unwrap();
    let headers = body
        .lines()
        .filter(|l| l.contains("norn_session_format"))
        .count();
    assert_eq!(headers, 1);
}

#[test]
fn pre_header_file_still_loads() {
    let tmp = tempfile::tempdir().unwrap();
    // A format-0 file: event lines only, no header.
    let events = [user_msg("old one"), user_msg("old two")];
    let mut body = String::new();
    for event in &events {
        body.push_str(&serde_json::to_string(event).unwrap());
        body.push('\n');
    }
    fs::write(session_file_path(tmp.path(), "legacy"), body).unwrap();

    let read = read_session_events(tmp.path(), "legacy").unwrap();
    assert_eq!(read.format_version, None, "no header => pre-versioning");
    assert_eq!(read.events.len(), 2);
    assert_eq!(read.skipped_lines, 0);
}

#[test]
fn create_session_stamps_writer_format_version() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    assert_eq!(entry.format_version, SESSION_FORMAT_VERSION);
    let index = read_index(tmp.path()).unwrap();
    assert_eq!(index[0].format_version, SESSION_FORMAT_VERSION);
}

#[test]
fn index_entry_without_format_version_defaults_to_zero() {
    let json = r#"{"id":"s","name":null,"model":"m","working_dir":"/w",
        "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
        "event_count":0,"status":"active"}"#;
    let entry: SessionIndexEntry = serde_json::from_str(json).unwrap();
    assert_eq!(entry.format_version, 0);
}

/// H19 regression: a torn FINAL line (ENOSPC / `kill -9` mid-write) must
/// be skipped with a count — never brick the whole session.
#[test]
fn torn_final_line_is_skipped_and_resume_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let events: Vec<_> = one_of_each().into_iter().take(3).collect();
    append_events(tmp.path(), &entry.id, &events, false).unwrap();

    // Tear the file: a partial JSON object with no trailing newline.
    let path = session_file_path(tmp.path(), &entry.id);
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(br#"{"type":"assistant_message","content":"trunc"#)
        .unwrap();
    drop(file);

    let read = read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(read.events.len(), 3, "intact events must all load");
    assert_eq!(read.skipped_lines, 1, "the torn line is counted");

    let resumed = manager(tmp.path())
        .resume(&entry.id, DurabilityPolicy::Flush)
        .unwrap();
    assert_eq!(resumed.store.len(), 3);
    assert_eq!(resumed.replay.replayed_events, 3);
    assert_eq!(resumed.replay.skipped_lines, 1);
}

/// R4 regression: an event variant from a newer writer is skipped with a
/// warning; everything else still loads.
#[test]
fn unknown_variant_line_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &[user_msg("known")], false).unwrap();

    let path = session_file_path(tmp.path(), &entry.id);
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"{\"type\":\"hologram_sync\",\"data\":42}\n")
        .unwrap();
    drop(file);
    append_events(tmp.path(), &entry.id, &[user_msg("after")], false).unwrap();

    let read = read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(read.events.len(), 2, "events around the unknown line load");
    assert_eq!(read.skipped_lines, 1);
}

/// A corrupt MIDDLE line must not lose the events after it.
#[test]
fn corrupt_middle_line_is_skipped_with_count() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &[user_msg("first")], false).unwrap();
    let path = session_file_path(tmp.path(), &entry.id);
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"not-json\n").unwrap();
    drop(file);
    append_events(tmp.path(), &entry.id, &[user_msg("second")], false).unwrap();

    let read = read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(read.events.len(), 2);
    assert_eq!(read.skipped_lines, 1);
}

#[test]
fn empty_lines_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let head: Vec<_> = one_of_each().into_iter().take(1).collect();
    append_events(tmp.path(), &entry.id, &head, false).unwrap();
    let path = session_file_path(tmp.path(), &entry.id);
    let body = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("\n   \n{body}\n  \n")).unwrap();
    let read = read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(read.events.len(), 1);
    assert_eq!(read.skipped_lines, 0);
}

#[test]
fn empty_file_returns_empty_vec() {
    let tmp = tempfile::tempdir().unwrap();
    let path = session_file_path(tmp.path(), "missing");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "").unwrap();
    let read = read_session_events(tmp.path(), "missing").unwrap();
    assert!(read.events.is_empty());
    assert_eq!(read.format_version, None);
}

#[test]
fn missing_file_returns_empty_vec() {
    let tmp = tempfile::tempdir().unwrap();
    let read = read_session_events(tmp.path(), "does-not-exist").unwrap();
    assert!(read.events.is_empty());
    assert_eq!(read.skipped_lines, 0);
}

// ----- R3: index maintenance -----

#[test]
fn append_three_events_leaves_index_count_three() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let events: Vec<_> = one_of_each().into_iter().take(3).collect();
    append_events(tmp.path(), &entry.id, &events, false).unwrap();
    let index = read_index(tmp.path()).unwrap();
    assert_eq!(index.len(), 1);
    assert_eq!(index[0].event_count, 3);
}

#[test]
fn index_jsonl_each_line_parses() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = fresh_session(tmp.path());
    let _ = fresh_session(tmp.path());
    let _ = fresh_session(tmp.path());
    let body = fs::read_to_string(index_file_path(tmp.path())).unwrap();
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let _: SessionIndexEntry = serde_json::from_str(line).unwrap();
    }
}

#[test]
fn no_stale_tmp_files_after_successful_atomic_write() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &one_of_each(), false).unwrap();
    let stale: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("index.jsonl.tmp")
        })
        .collect();
    assert!(stale.is_empty(), "stale tmp files remain: {stale:?}");
}

#[test]
fn stray_tmp_file_does_not_corrupt_canonical_index() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let canonical_before = fs::read(index_file_path(tmp.path())).unwrap();
    // Drop a bogus .tmp file to mimic a previous crash mid-write.
    fs::write(tmp.path().join("index.jsonl.tmp.stale"), "garbage\n").unwrap();
    // Canonical file is unaffected; subsequent reads still succeed.
    let index = read_index(tmp.path()).unwrap();
    assert_eq!(index.len(), 1);
    assert_eq!(index[0].id, entry.id);
    let canonical_after = fs::read(index_file_path(tmp.path())).unwrap();
    assert_eq!(canonical_before, canonical_after);
}

#[test]
fn three_sessions_listed_with_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let a = fresh_session(tmp.path());
    let b = fresh_session(tmp.path());
    let c = fresh_session(tmp.path());
    let index = read_index(tmp.path()).unwrap();
    let ids: Vec<&str> = index.iter().map(|e| e.id.as_str()).collect();
    assert!(ids.contains(&a.id.as_str()));
    assert!(ids.contains(&b.id.as_str()));
    assert!(ids.contains(&c.id.as_str()));
    for e in &index {
        assert_eq!(e.model, "gpt-x");
        assert_eq!(e.working_dir, "/work");
        assert_eq!(e.status, SessionStatus::Active);
    }
}

// ----- R4: append protocol -----

#[test]
fn two_batches_sum_line_count() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let first: Vec<_> = one_of_each().into_iter().take(4).collect();
    let second: Vec<_> = one_of_each().into_iter().skip(4).take(2).collect();
    append_events(tmp.path(), &entry.id, &first, false).unwrap();
    append_events(tmp.path(), &entry.id, &second, false).unwrap();
    let body = fs::read_to_string(session_file_path(tmp.path(), &entry.id)).unwrap();
    let line_count = body.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(line_count, 7, "header line + six event lines");
    let index = read_index(tmp.path()).unwrap();
    assert_eq!(index[0].event_count, 6);
}

#[test]
fn disabled_append_leaves_filesystem_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    // No fresh_session call -> no index file yet.
    append_events(
        tmp.path(),
        "abcdef12-3456-7890-abcd-ef1234567890",
        &one_of_each(),
        true,
    )
    .unwrap();
    assert!(!index_file_path(tmp.path()).exists());
    assert!(!session_file_path(tmp.path(), "abcdef12-3456-7890-abcd-ef1234567890").exists());
}

#[test]
fn append_creates_missing_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let nested = tmp.path().join("nested").join("deeper");
    let entry = index_only_entry(&nested);
    let only = vec![user_msg("hi")];
    append_events(&nested, &entry.id, &only, false).unwrap();
    assert!(session_file_path(&nested, &entry.id).exists());
}

#[test]
fn append_does_not_overwrite_existing_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &[user_msg("a")], false).unwrap();
    append_events(tmp.path(), &entry.id, &[user_msg("b")], false).unwrap();
    let body = fs::read_to_string(session_file_path(tmp.path(), &entry.id)).unwrap();
    assert!(body.contains("\"a\""));
    assert!(body.contains("\"b\""));
}

#[test]
fn append_to_unknown_session_id_is_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = fresh_session(tmp.path());
    let err = append_events(tmp.path(), "ghost", &[user_msg("x")], false).unwrap_err();
    assert!(matches!(err, SessionPersistError::NotFound { .. }));
    assert!(
        !session_file_path(tmp.path(), "ghost").exists(),
        "no event bytes may land for a session the index does not know"
    );
}

// ----- R5: resume -----

#[test]
fn resume_reconstructs_event_store() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let events: Vec<_> = one_of_each().into_iter().take(5).collect();
    append_events(tmp.path(), &entry.id, &events, false).unwrap();

    let resumed = manager(tmp.path())
        .resume(&entry.id, DurabilityPolicy::Flush)
        .unwrap();
    assert_eq!(resumed.store.len(), 5);
    assert_eq!(resumed.replay.replayed_events, 5);
    assert_eq!(resumed.entry.id, entry.id);
    let store_events = resumed.store.events();
    for (a, b) in events.iter().zip(store_events.iter()) {
        assert_event_eq(a, b);
    }
}

#[test]
fn resume_empty_string_resolves_latest_updated() {
    let tmp = tempfile::tempdir().unwrap();
    let a = fresh_session(tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(5));
    let b = fresh_session(tmp.path());
    // Touch `a`'s entry with an event to bump its updated_at past `b`.
    append_events(tmp.path(), &a.id, &[user_msg("late")], false).unwrap();

    let resumed = manager(tmp.path())
        .resume("", DurabilityPolicy::Flush)
        .unwrap();
    assert_eq!(
        resumed.entry.id, a.id,
        "expected `a` (most recently updated), not `b={}`",
        b.id
    );
}

#[test]
fn resolve_latest_in_working_dir_ignores_newer_other_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let current_dir = fresh_session_at(tmp.path(), "/repo/current");
    std::thread::sleep(std::time::Duration::from_millis(5));
    let other_dir = fresh_session_at(tmp.path(), "/repo/other");

    let resolved =
        resolve_latest_session_in_working_dir(tmp.path(), Path::new("/repo/current")).unwrap();
    assert_eq!(
        resolved.id, current_dir.id,
        "expected current-dir session, not globally newer other-dir session {}",
        other_dir.id,
    );
}

#[test]
fn resolve_latest_in_working_dir_uses_canonical_match_when_available() {
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    fs::create_dir(&project).unwrap();
    let stored_path = project.join(".");
    let entry = fresh_session_at(tmp.path(), &stored_path.to_string_lossy());

    let resolved = resolve_latest_session_in_working_dir(tmp.path(), &project).unwrap();
    assert_eq!(resolved.id, entry.id);
}

#[test]
fn resolve_latest_in_working_dir_reports_not_found_for_unmatched_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = fresh_session_at(tmp.path(), "/repo/other");

    let err =
        resolve_latest_session_in_working_dir(tmp.path(), Path::new("/repo/current")).unwrap_err();
    assert!(matches!(err, SessionPersistError::NotFound { .. }));
}

#[test]
fn resume_eight_char_unique_prefix_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &[user_msg("hi")], false).unwrap();
    let prefix = &entry.id[..8];
    let resumed = manager(tmp.path())
        .resume(prefix, DurabilityPolicy::Flush)
        .unwrap();
    assert_eq!(resumed.entry.id, entry.id);
}

#[test]
fn resume_unknown_prefix_returns_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = fresh_session(tmp.path());
    let err = manager(tmp.path())
        .resume("ffffffff-no-match", DurabilityPolicy::Flush)
        .unwrap_err();
    assert!(matches!(err, SessionPersistError::NotFound { .. }));
}

#[test]
fn resume_ambiguous_prefix_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    // Synthesise two index rows that share an 8-character prefix.
    let now = Utc::now();
    let shared_prefix = "abcdef12";
    let mut entries = Vec::new();
    for tail in ["3456-7890-abcd-ef1234567890", "3456-7890-abcd-ef1234567891"] {
        entries.push(SessionIndexEntry {
            id: format!("{shared_prefix}-{tail}"),
            name: None,
            model: "gpt".to_owned(),
            working_dir: "/w".to_owned(),
            created_at: now,
            updated_at: now,
            event_count: 0,
            status: SessionStatus::Active,
            format_version: SESSION_FORMAT_VERSION,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
            rel_path: None,
            parent_id: None,
        });
    }
    write_index_atomic(tmp.path(), &entries).unwrap();
    let err = manager(tmp.path())
        .resume(shared_prefix, DurabilityPolicy::Flush)
        .unwrap_err();
    match err {
        SessionPersistError::AmbiguousPrefix { matches, .. } => {
            assert_eq!(matches.len(), 2);
        }
        other => panic!("expected AmbiguousPrefix, got {other:?}"),
    }
}

#[test]
fn resume_too_short_prefix_returns_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let err = manager(tmp.path())
        .resume(&entry.id[..7], DurabilityPolicy::Flush)
        .unwrap_err();
    assert!(matches!(err, SessionPersistError::NotFound { .. }));
}

#[test]
fn resume_by_name_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = manager(tmp.path())
        .create(
            options("gpt", "/w", Some("nightly")),
            DurabilityPolicy::Flush,
        )
        .unwrap()
        .entry;
    append_events(tmp.path(), &entry.id, &[user_msg("hi")], false).unwrap();
    let resumed = manager(tmp.path())
        .resume("nightly", DurabilityPolicy::Flush)
        .unwrap();
    assert_eq!(resumed.entry.id, entry.id);
}

#[test]
fn resume_empty_index_returns_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let err = manager(tmp.path())
        .resume("", DurabilityPolicy::Flush)
        .unwrap_err();
    assert!(matches!(err, SessionPersistError::NotFound { .. }));
}

// ----- R6: fork -----

#[test]
fn fork_appends_fork_event_at_tail() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let events: Vec<_> = one_of_each().into_iter().take(5).collect();
    append_events(tmp.path(), &entry.id, &events, false).unwrap();

    let fork = manager(tmp.path())
        .fork(
            &entry.id,
            options("gpt", "/w", None),
            DurabilityPolicy::Flush,
        )
        .unwrap();
    assert_eq!(fork.replay.replayed_events, 6);
    assert_eq!(fork.store.len(), 6);

    let body = fs::read_to_string(session_file_path(tmp.path(), &fork.entry.id)).unwrap();
    let line_count = body.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(line_count, 7, "header line + six event lines");
}

#[test]
fn fork_event_source_id_matches_last_original() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let events: Vec<_> = one_of_each().into_iter().take(3).collect();
    append_events(tmp.path(), &entry.id, &events, false).unwrap();
    let last_id = events.last().unwrap().base().id.clone();

    let fork = manager(tmp.path())
        .fork(
            &entry.id,
            options("gpt", "/w", None),
            DurabilityPolicy::Flush,
        )
        .unwrap();
    let all_events = fork.store.events();
    match all_events.last().unwrap() {
        SessionEvent::ChildBranch {
            parent_session_id,
            child_session_id,
            parent_event_anchor,
            kind,
            ..
        } => {
            assert_eq!(parent_event_anchor.as_ref(), Some(&last_id));
            assert_eq!(parent_session_id.as_deref(), Some(entry.id.as_str()));
            assert_eq!(child_session_id.as_deref(), Some(fork.entry.id.as_str()));
            assert_eq!(*kind, ChildBranchKind::Fork);
        }
        other => panic!("expected ChildBranch tail, got {other:?}"),
    }
}

#[test]
fn fork_does_not_modify_source_file() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &one_of_each(), false).unwrap();
    let source_path = session_file_path(tmp.path(), &entry.id);
    let before = fs::read(&source_path).unwrap();
    let _ = manager(tmp.path())
        .fork(
            &entry.id,
            options("gpt", "/w", None),
            DurabilityPolicy::Flush,
        )
        .unwrap();
    let after = fs::read(&source_path).unwrap();
    assert_eq!(before, after);
}

#[test]
fn fork_index_contains_both_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &one_of_each(), false).unwrap();
    let fork = manager(tmp.path())
        .fork(
            &entry.id,
            options("gpt", "/w", None),
            DurabilityPolicy::Flush,
        )
        .unwrap();
    let ids: Vec<String> = read_index(tmp.path())
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert!(ids.contains(&entry.id));
    assert!(ids.contains(&fork.entry.id));
}

#[test]
fn fork_empty_source_returns_empty_source() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let err = manager(tmp.path())
        .fork(
            &entry.id,
            options("gpt", "/w", None),
            DurabilityPolicy::Flush,
        )
        .unwrap_err();
    assert!(matches!(err, SessionPersistError::EmptySource { .. }));
}

#[test]
fn fork_no_argument_resolves_latest() {
    let tmp = tempfile::tempdir().unwrap();
    let _older = fresh_session(tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(5));
    let newer = fresh_session(tmp.path());
    append_events(tmp.path(), &newer.id, &one_of_each(), false).unwrap();
    let fork = manager(tmp.path())
        .fork("", options("gpt", "/w", None), DurabilityPolicy::Flush)
        .unwrap();
    assert_eq!(fork.replay.replayed_events, one_of_each().len() + 1);
    // The forked session is a new entry -- not the newer source.
    assert_ne!(fork.entry.id, newer.id);
}

// ----- update_session_index: index-only reconcile (double-write fix) -----

#[test]
fn update_session_index_adds_count_and_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let usage = Usage {
        input_tokens: 10,
        output_tokens: 20,
        cache_read_tokens: 5,
        ..Usage::default()
    };
    update_session_index(tmp.path(), &entry.id, 3, &usage, None).unwrap();

    let index = read_index(tmp.path()).unwrap();
    assert_eq!(index.len(), 1);
    assert_eq!(index[0].event_count, 3);
    assert_eq!(index[0].total_input_tokens, 10);
    assert_eq!(index[0].total_output_tokens, 20);
    assert_eq!(index[0].total_cache_read_tokens, 5);
}

#[test]
fn update_session_index_does_not_write_session_jsonl() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = index_only_entry(tmp.path());
    update_session_index(tmp.path(), &entry.id, 4, &Usage::default(), None).unwrap();
    // The index-only path must never create or touch the session JSONL.
    assert!(!session_file_path(tmp.path(), &entry.id).exists());
}

#[test]
fn update_session_index_accumulates_across_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    update_session_index(tmp.path(), &entry.id, 2, &assistant_usage(7, 3, 1), None).unwrap();
    update_session_index(tmp.path(), &entry.id, 5, &assistant_usage(4, 6, 2), None).unwrap();

    let index = read_index(tmp.path()).unwrap();
    assert_eq!(index[0].event_count, 7);
    assert_eq!(index[0].total_input_tokens, 11);
    assert_eq!(index[0].total_output_tokens, 9);
    assert_eq!(index[0].total_cache_read_tokens, 3);
}

#[test]
fn update_session_index_zero_count_and_usage_is_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let created_updated_at = read_index(tmp.path()).unwrap()[0].updated_at;
    // No events, no tokens -> must not touch the index at all.
    update_session_index(tmp.path(), &entry.id, 0, &Usage::default(), None).unwrap();
    let index = read_index(tmp.path()).unwrap();
    assert_eq!(index[0].event_count, 0);
    assert_eq!(index[0].updated_at, created_updated_at);
}

#[test]
fn update_session_index_unknown_session_is_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = fresh_session(tmp.path());
    let err =
        update_session_index(tmp.path(), "ghost", 1, &assistant_usage(1, 0, 0), None).unwrap_err();
    assert!(matches!(err, SessionPersistError::NotFound { .. }));
}

#[test]
fn sum_usage_from_events_sums_assistant_only() {
    let events = vec![
        user_msg("a"),
        assistant_with_usage(10, 20, 5),
        user_msg("b"),
        assistant_with_usage(1, 2, 3),
    ];
    let total = sum_usage_from_events(&events);
    assert_eq!(total.input_tokens, 11);
    assert_eq!(total.output_tokens, 22);
    assert_eq!(total.cache_read_tokens, 8);
}

#[test]
fn sum_usage_from_events_empty_is_zero() {
    let total = sum_usage_from_events(&[]);
    assert_eq!(total.input_tokens, 0);
    assert_eq!(total.output_tokens, 0);
    assert_eq!(total.cache_read_tokens, 0);
}

/// End-to-end regression for the double-write bug and the
/// agent-maintained index: the write-through `JsonlSink` every manager
/// open installs must leave the session JSONL at a 1:1 line-to-event
/// ratio, bring the index entry in step at the turn-boundary
/// `checkpoint` **without any manual `update_session_index` call**, and
/// remain resumable.
#[test]
fn registered_sink_maintains_index_and_stays_resumable() {
    let tmp = tempfile::tempdir().unwrap();
    let opened = manager(tmp.path())
        .create(options("gpt-x", "/work", None), DurabilityPolicy::Flush)
        .unwrap();
    let entry_id = opened.entry.id.clone();
    let created_updated_at = read_index(tmp.path()).unwrap()[0].updated_at;

    // Simulate a turn: write events through the sink (write-through),
    // then checkpoint at the turn boundary.
    let turn = vec![user_msg("hello"), assistant_with_usage(12, 8, 4)];
    for event in &turn {
        opened.store.append(event.clone()).unwrap();
    }
    opened.store.checkpoint().unwrap();

    // JSONL holds the header plus exactly the turn's events.
    let body = fs::read_to_string(session_file_path(tmp.path(), &entry_id)).unwrap();
    let line_count = body.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(
        line_count,
        turn.len() + 1,
        "expected header + 1:1 lines, got double-write"
    );

    // Index reflects the turn with NO manual reconcile call.
    let index = read_index(tmp.path()).unwrap();
    assert_eq!(index[0].event_count, 2);
    assert_eq!(index[0].total_input_tokens, 12);
    assert_eq!(index[0].total_output_tokens, 8);
    assert_eq!(index[0].total_cache_read_tokens, 4);
    assert!(
        index[0].updated_at > created_updated_at,
        "updated_at must advance on append"
    );

    // The session resumes cleanly — the duplicate-ID guard never fires.
    let resumed = manager(tmp.path())
        .resume(&entry_id, DurabilityPolicy::Flush)
        .unwrap();
    assert_eq!(resumed.store.len(), 2);
    assert_eq!(resumed.replay.replayed_events, 2);
}

/// Opening a session must surface sink-open failures instead of
/// silently degrading to memory-only persistence.
#[test]
fn sink_open_failure_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = index_only_entry(tmp.path());
    // Occupy the session file path with a directory so the open fails.
    fs::create_dir_all(session_file_path(tmp.path(), &entry.id)).unwrap();

    let result = manager(tmp.path()).resume(&entry.id, DurabilityPolicy::Flush);
    assert!(result.is_err(), "open failure must not be swallowed");
}

/// Exercise the explicit durability policies through a raw sink.
#[test]
fn durability_policies_persist_every_event() {
    let tmp = tempfile::tempdir().unwrap();
    for (name, durability) in [
        ("flush", DurabilityPolicy::Flush),
        ("per-event", DurabilityPolicy::FsyncPerEvent),
        (
            "every-2",
            DurabilityPolicy::FsyncEveryEvents(std::num::NonZeroU64::new(2).unwrap()),
        ),
    ] {
        let path = session_file_path(tmp.path(), name);
        let mut sink = JsonlSink::open_with(&path, durability).unwrap();
        for i in 0..3 {
            sink.persist(&user_msg(&format!("{name}-{i}"))).unwrap();
        }
        drop(sink);
        let body = fs::read_to_string(&path).unwrap();
        assert_eq!(body.lines().count(), 4, "{name}: header + 3 events");
    }
}

// ----- Torn-line healing across reopen (H19, reopen half) -----

/// A torn final line (crash mid-write) must be healed when the file is
/// reopened for appending via the batch path: the next appended event
/// must land on its own line, never concatenated onto the torn bytes.
#[test]
fn torn_final_line_is_healed_on_batch_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(
        tmp.path(),
        &entry.id,
        &[user_msg("one"), user_msg("two")],
        false,
    )
    .unwrap();

    // Tear the file the way ENOSPC / `kill -9` would: partial JSON, no
    // trailing newline.
    let path = session_file_path(tmp.path(), &entry.id);
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(br#"{"type":"user_message","content":"torn"#)
        .unwrap();
    drop(file);

    // "Next process" appends after the crash.
    append_events(tmp.path(), &entry.id, &[user_msg("after-crash")], false).unwrap();

    let read = read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(
        read.events.len(),
        3,
        "the post-crash append must parse — the torn line must not absorb it"
    );
    assert_eq!(
        read.skipped_lines, 1,
        "the torn line is exactly one skipped line"
    );
    match read.events.last().unwrap() {
        SessionEvent::UserMessage { content, .. } => assert_eq!(content, "after-crash"),
        other => panic!("expected the post-crash user message last, got {other:?}"),
    }
}

/// Same crash scenario through the live-sink path: a manager resume
/// after a torn final line must heal the tear before the first
/// write-through append.
#[test]
fn torn_final_line_is_healed_on_sink_reopen() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &[user_msg("one")], false).unwrap();

    let path = session_file_path(tmp.path(), &entry.id);
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(br#"{"type":"assistant_message","content":"tor"#)
        .unwrap();
    drop(file);

    let resumed = manager(tmp.path())
        .resume(&entry.id, DurabilityPolicy::Flush)
        .unwrap();
    assert_eq!(
        resumed.replay.replayed_events, 1,
        "torn line skipped on resume"
    );
    assert_eq!(resumed.replay.skipped_lines, 1);
    resumed.store.append(user_msg("after-crash")).unwrap();
    drop(resumed);

    let read = read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(
        read.events.len(),
        2,
        "sink reopen must heal the tear so the new event parses"
    );
    assert_eq!(read.skipped_lines, 1);
}

// ----- Duplicate EventId tolerance (crash-retry artifacts) -----

/// A duplicated event line (the documented-safe retry after a failure
/// that actually persisted the first attempt) must be skipped on read:
/// first occurrence kept, later occurrences counted like other skipped
/// lines.
#[test]
fn duplicate_event_lines_are_skipped_on_read() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let a = user_msg("a");
    append_events(tmp.path(), &entry.id, std::slice::from_ref(&a), false).unwrap();
    // Retry artifact: the exact same event appended again.
    append_events(tmp.path(), &entry.id, std::slice::from_ref(&a), false).unwrap();
    append_events(tmp.path(), &entry.id, &[user_msg("b")], false).unwrap();

    let read = read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(
        read.events.len(),
        2,
        "duplicate must be dropped, first kept"
    );
    assert_eq!(
        read.skipped_lines, 1,
        "duplicate accounted like a skipped line"
    );
    assert_event_eq(&read.events[0], &a);
}

/// Resume and fork must both survive a duplicated event line instead of
/// hard-failing on the `EventStore` duplicate-ID guard (which made a
/// transient hiccup permanently brick the session).
#[test]
fn resume_and_fork_tolerate_duplicate_event_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let a = user_msg("a");
    append_events(tmp.path(), &entry.id, std::slice::from_ref(&a), false).unwrap();
    append_events(tmp.path(), &entry.id, std::slice::from_ref(&a), false).unwrap();
    append_events(tmp.path(), &entry.id, &[user_msg("b")], false).unwrap();

    let resumed = manager(tmp.path())
        .resume(&entry.id, DurabilityPolicy::Flush)
        .unwrap();
    assert_eq!(resumed.store.len(), 2);
    assert_eq!(resumed.replay.replayed_events, 2);
    drop(resumed);

    let fork = manager(tmp.path())
        .fork(
            &entry.id,
            options("gpt", "/w", None),
            DurabilityPolicy::Flush,
        )
        .unwrap();
    assert_eq!(
        fork.replay.replayed_events, 3,
        "2 deduplicated events + Fork tail"
    );
    assert_eq!(fork.store.len(), 3);
}

// ----- Index failure after a durable event write (retry-safety) -----

/// When the event bytes are already durable, a failure to update the
/// index must NOT fail the append (the documented-safe retry would write
/// a duplicate line). The index goes stale and is repaired on resume.
#[cfg(unix)]
#[test]
fn append_events_index_failure_is_durable_and_nonfatal() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &[user_msg("one")], false).unwrap();

    // Make the data dir read-only: the session file and index.lock
    // already exist (writable), but the atomic index rewrite cannot
    // create its tmp file.
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let result = append_events(tmp.path(), &entry.id, &[user_msg("two")], false);
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o755)).unwrap();

    result.expect("append must report success: the event IS durable");
    let read = read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(
        read.events.len(),
        2,
        "the event landed despite the index failure"
    );

    // The index is stale (still 1) until resume repairs it.
    assert_eq!(read_index(tmp.path()).unwrap()[0].event_count, 1);
    let resumed = manager(tmp.path())
        .resume(&entry.id, DurabilityPolicy::Flush)
        .unwrap();
    assert_eq!(
        resumed.entry.event_count, 2,
        "resume must repair the stale entry"
    );
    assert_eq!(read_index(tmp.path()).unwrap()[0].event_count, 2);
}

/// Sink path: an index failure after a durable write-through append must
/// not fail the append; the delta is retained and lands at the next
/// checkpoint.
#[cfg(unix)]
#[test]
fn sink_index_failure_retains_delta_and_recovers_on_checkpoint() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let store = manager(tmp.path())
        .create(
            options("gpt-x", "/work", None),
            DurabilityPolicy::FsyncPerEvent,
        )
        .unwrap()
        .store;

    store.append(user_msg("one")).unwrap();
    assert_eq!(read_index(tmp.path()).unwrap()[0].event_count, 1);

    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let result = store.append(user_msg("two"));
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o755)).unwrap();
    result.expect("append must succeed: the event is durable, only the index lagged");
    assert_eq!(store.len(), 2, "event must be visible in memory");
    assert_eq!(
        read_index(tmp.path()).unwrap()[0].event_count,
        1,
        "index is stale after the failure"
    );

    store
        .checkpoint()
        .expect("checkpoint retries the retained delta");
    assert_eq!(read_index(tmp.path()).unwrap()[0].event_count, 2);
}

// ----- Index batching at durability boundaries -----

/// Under `DurabilityPolicy::Flush` the index delta is deferred: no index
/// rewrite (and no fsync) per event. `checkpoint()` and `Drop` are the
/// flush points.
#[test]
fn flush_policy_defers_index_updates_to_checkpoint_and_drop() {
    let tmp = tempfile::tempdir().unwrap();
    let store = manager(tmp.path())
        .create(options("gpt-x", "/work", None), DurabilityPolicy::Flush)
        .unwrap()
        .store;

    store.append(user_msg("one")).unwrap();
    store.append(assistant_with_usage(12, 8, 4)).unwrap();
    assert_eq!(
        read_index(tmp.path()).unwrap()[0].event_count,
        0,
        "Flush must not rewrite the index per event"
    );

    store.checkpoint().unwrap();
    let index = read_index(tmp.path()).unwrap();
    assert_eq!(index[0].event_count, 2);
    assert_eq!(index[0].total_input_tokens, 12);
    assert_eq!(index[0].total_output_tokens, 8);
    assert_eq!(index[0].total_cache_read_tokens, 4);

    store.append(user_msg("three")).unwrap();
    assert_eq!(read_index(tmp.path()).unwrap()[0].event_count, 2);
    drop(store);
    assert_eq!(
        read_index(tmp.path()).unwrap()[0].event_count,
        3,
        "drop (clean shutdown) must flush the pending delta"
    );
}

/// `FsyncEveryEvents(n)`: the index catches up exactly at each event
/// fsync boundary, and any tail delta lands on drop.
#[test]
fn fsync_every_n_flushes_index_at_durability_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    let store = manager(tmp.path())
        .create(
            options("gpt-x", "/work", None),
            DurabilityPolicy::FsyncEveryEvents(std::num::NonZeroU64::new(2).unwrap()),
        )
        .unwrap()
        .store;

    store.append(user_msg("one")).unwrap();
    assert_eq!(read_index(tmp.path()).unwrap()[0].event_count, 0);
    store.append(user_msg("two")).unwrap();
    assert_eq!(read_index(tmp.path()).unwrap()[0].event_count, 2);
    store.append(user_msg("three")).unwrap();
    assert_eq!(read_index(tmp.path()).unwrap()[0].event_count, 2);
    drop(store);
    assert_eq!(read_index(tmp.path()).unwrap()[0].event_count, 3);
}

/// `FsyncPerEvent`: every event is a durability boundary, so the index
/// stays current per event.
#[test]
fn fsync_per_event_keeps_index_current() {
    let tmp = tempfile::tempdir().unwrap();
    let store = manager(tmp.path())
        .create(
            options("gpt-x", "/work", None),
            DurabilityPolicy::FsyncPerEvent,
        )
        .unwrap()
        .store;
    store.append(user_msg("one")).unwrap();
    assert_eq!(read_index(tmp.path()).unwrap()[0].event_count, 1);
    store.append(user_msg("two")).unwrap();
    assert_eq!(read_index(tmp.path()).unwrap()[0].event_count, 2);
}

// ----- Resume-time index self-maintenance -----

/// A crash with deferred index deltas (or a lost delta after an index
/// failure) leaves the entry stale; resume must recompute `event_count`
/// and usage totals from the event file and repair the entry.
#[test]
fn resume_repairs_stale_index_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let events = vec![
        user_msg("one"),
        assistant_with_usage(10, 5, 2),
        user_msg("two"),
    ];
    append_events(tmp.path(), &entry.id, &events, false).unwrap();

    // Simulate crash staleness: zero the entry behind persistence's back.
    update_index_entry(tmp.path(), &entry.id, None, |e| {
        e.event_count = 0;
        e.total_input_tokens = 0;
        e.total_output_tokens = 0;
        e.total_cache_read_tokens = 0;
    })
    .unwrap();

    let resolved = manager(tmp.path())
        .resume(&entry.id, DurabilityPolicy::Flush)
        .unwrap()
        .entry;
    assert_eq!(
        resolved.event_count, 3,
        "resolved entry carries repaired count"
    );
    assert_eq!(resolved.total_input_tokens, 10);
    assert_eq!(resolved.total_output_tokens, 5);
    assert_eq!(resolved.total_cache_read_tokens, 2);

    let index = read_index(tmp.path()).unwrap();
    assert_eq!(index[0].event_count, 3, "repair is persisted to the index");
    assert_eq!(index[0].total_input_tokens, 10);
    assert_eq!(index[0].total_output_tokens, 5);
    assert_eq!(index[0].total_cache_read_tokens, 2);
}

// ----- H18: inter-process index locking -----

/// Regression for H18: concurrent `O_APPEND` creates racing
/// read-modify-rewrite updates must never drop an index entry. Without
/// the advisory lock, an update working from a stale snapshot rewrote
/// the index minus concurrently created sessions, making them
/// permanently unresumable.
#[test]
fn concurrent_creates_and_updates_drop_no_index_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let seed = fresh_session(tmp.path());
    let dir = tmp.path().to_path_buf();

    let updater = {
        let dir = dir.clone();
        let id = seed.id.clone();
        std::thread::spawn(move || {
            for _ in 0..40 {
                update_session_index(&dir, &id, 1, &Usage::default(), None).unwrap();
            }
        })
    };

    let creators: Vec<_> = (0..4)
        .map(|_| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let mgr = SessionManager::new(&dir);
                (0..10)
                    .map(|_| {
                        mgr.create(
                            CreateSessionOptions {
                                model: "gpt-x".to_owned(),
                                working_dir: "/w".to_owned(),
                                name: None,
                            },
                            DurabilityPolicy::Flush,
                        )
                        .unwrap()
                        .entry
                        .id
                    })
                    .collect::<Vec<String>>()
            })
        })
        .collect();

    let mut created: Vec<String> = vec![seed.id.clone()];
    for handle in creators {
        created.extend(handle.join().unwrap());
    }
    updater.join().unwrap();

    let index = read_index(tmp.path()).unwrap();
    let ids: std::collections::HashSet<&str> = index.iter().map(|e| e.id.as_str()).collect();
    for id in &created {
        assert!(
            ids.contains(id.as_str()),
            "index entry for {id} was dropped by a concurrent rewrite"
        );
    }
    assert_eq!(index.len(), created.len());
    let seed_entry = index.iter().find(|e| e.id == seed.id).unwrap();
    assert_eq!(seed_entry.event_count, 40, "no update was lost either");
}

/// Concurrent registered sinks (two stores, same data dir — the
/// multi-process topology meridian runs, simulated in-process) must keep
/// both index entries intact and correctly counted.
#[test]
fn two_sink_backed_stores_same_dir_do_not_corrupt_index() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = manager(tmp.path());
    let opened_a = mgr
        .create(options("gpt-x", "/work", None), DurabilityPolicy::Flush)
        .unwrap();
    let opened_b = mgr
        .create(options("gpt-x", "/work", None), DurabilityPolicy::Flush)
        .unwrap();
    let (a_id, store_a) = (opened_a.entry.id, opened_a.store);
    let (b_id, store_b) = (opened_b.entry.id, opened_b.store);

    let writer_a = std::thread::spawn(move || {
        for i in 0..25 {
            store_a.append(user_msg(&format!("a{i}"))).unwrap();
        }
    });
    let writer_b = std::thread::spawn(move || {
        for i in 0..25 {
            store_b.append(user_msg(&format!("b{i}"))).unwrap();
        }
    });
    writer_a.join().unwrap();
    writer_b.join().unwrap();

    let index = read_index(tmp.path()).unwrap();
    assert_eq!(index.len(), 2);
    for entry in &index {
        assert_eq!(entry.event_count, 25, "entry {} miscounted", entry.id);
    }
    let resumed_a = mgr.resume(&a_id, DurabilityPolicy::Flush).unwrap();
    let resumed_b = mgr.resume(&b_id, DurabilityPolicy::Flush).unwrap();
    assert_eq!(resumed_a.store.len(), 25);
    assert_eq!(resumed_b.store.len(), 25);
}

// ----- Reserved session IDs (persistence-owned file names) -----

/// Blocker regression: ids map to `{id}.jsonl`, so the id `"index"`
/// collides with the session index itself. Every persistence boundary a
/// caller-supplied id can reach must reject the reserved name family —
/// not just the manager's validation.
#[test]
fn reserved_ids_rejected_at_every_persistence_boundary() {
    let tmp = tempfile::tempdir().unwrap();
    // A healthy index with one real session, so corruption would show.
    let real = fresh_session(tmp.path());
    let index_before = fs::read_to_string(index_file_path(tmp.path())).unwrap();

    let now = Utc::now();
    let smuggled = SessionIndexEntry {
        id: "index".to_owned(),
        name: None,
        model: "gpt-x".to_owned(),
        working_dir: "/work".to_owned(),
        created_at: now,
        updated_at: now,
        event_count: 0,
        status: SessionStatus::Active,
        format_version: SESSION_FORMAT_VERSION,
        total_input_tokens: 0,
        total_output_tokens: 0,
        total_cache_read_tokens: 0,
        rel_path: None,
        parent_id: None,
    };

    // Index insertion: a reserved id must never enter the index.
    let err = append_index_entry(tmp.path(), &smuggled, None).unwrap_err();
    assert!(
        matches!(err, SessionPersistError::InvalidSessionId { .. }),
        "append_index_entry must reject reserved ids, got {err:?}",
    );
    let err = insert_index_entry_if_absent(tmp.path(), &smuggled, None).unwrap_err();
    assert!(
        matches!(err, SessionPersistError::InvalidSessionId { .. }),
        "insert_index_entry_if_absent must reject reserved ids, got {err:?}",
    );

    // Event append: must never write session events into index.jsonl.
    let err = append_events(tmp.path(), "index", &[user_msg("evil")], false).unwrap_err();
    assert!(
        matches!(err, SessionPersistError::InvalidSessionId { .. }),
        "append_events must reject reserved ids, got {err:?}",
    );

    // Event read: must never parse index.jsonl as a session file.
    let err = read_session_events(tmp.path(), "index").unwrap_err();
    assert!(
        matches!(err, SessionPersistError::InvalidSessionId { .. }),
        "read_session_events must reject reserved ids, got {err:?}",
    );

    // Sink open: must never attach an event sink to index.jsonl.
    match JsonlSink::open_registered(tmp.path(), &smuggled, DurabilityPolicy::Flush, None) {
        Err(SessionPersistError::InvalidSessionId { .. }) => {}
        Err(other) => panic!("JsonlSink::open_registered wrong error: {other:?}"),
        Ok(_) => panic!("JsonlSink::open_registered must reject reserved ids"),
    }

    // Nothing above may have altered the index.
    let index_after = fs::read_to_string(index_file_path(tmp.path())).unwrap();
    assert_eq!(index_before, index_after, "index bytes untouched");
    assert_eq!(read_index(tmp.path()).unwrap()[0].id, real.id);
}

/// The reservation rule is a name *family*, not an enumeration: a stem
/// reserves itself plus every `.`-extended sibling, so any future
/// persistence-owned file named `index.<suffix>` is excluded
/// automatically.
#[test]
fn reserved_id_rule_covers_stem_family_only() {
    for reserved in [
        "index",
        "index.jsonl",
        "index.lock",
        "index.jsonl.tmp.deadbeef",
        "index.anything-future",
        // Case-insensitive: the default macOS / Windows filesystems are
        // case-insensitive, so "INDEX.jsonl" IS "index.jsonl" there.
        "INDEX",
        "Index.Lock",
    ] {
        assert!(
            io::is_reserved_session_id(reserved),
            "{reserved:?} must be reserved",
        );
    }
    for free in ["indexer", "myindex", "ind", "index-2", "index_2"] {
        assert!(
            !io::is_reserved_session_id(free),
            "{free:?} must stay claimable",
        );
    }
}

// ----- Single-pass replay (ReplayArtifacts) --------------------------------

/// `Read` wrapper counting every byte served to the tolerant reader.
struct CountingReader<R> {
    inner: R,
    bytes_served: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl<R: std::io::Read> std::io::Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes_served
            .fetch_add(n, std::sync::atomic::Ordering::Relaxed);
        Ok(n)
    }
}

/// A representative session history: header, a user message, an
/// assistant message carrying usage and an envelope-bearing tool call,
/// the matching tool result, a compaction superseding the user message,
/// and one torn (corrupt) trailing line.
fn replay_fixture() -> (Vec<u8>, EventId) {
    let user = user_msg("hello");
    let superseded_id = user.base().id.clone();
    let assistant = SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        content: String::new(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: vec![ToolCallEvent {
            call_id: "call_replay_1".to_owned(),
            name: "read".to_owned(),
            arguments: serde_json::json!({
                "path": "src/a.rs",
                "tool_use_description": "inspect module a",
            }),
            kind: crate::provider::request::ToolCallKind::Function,
        }],
        usage: EventUsage {
            input_tokens: 11,
            output_tokens: 7,
            cache_read_tokens: 3,
            cache_write_tokens: 0,
            cost_usd: None,
        },
        stop_reason: "tool_use".to_owned(),
        response_id: None,
    };
    let result = SessionEvent::ToolResult {
        base: EventBase::new(None),
        tool_call_id: "call_replay_1".to_owned(),
        tool_name: "read".to_owned(),
        output: serde_json::json!({"lines": 3}),
        duration_ms: 5,
    };
    let compaction = SessionEvent::Compaction {
        base: EventBase::new(None),
        summary: "compacted".to_owned(),
        replaced_event_ids: vec![superseded_id.clone()],
    };

    let mut data = Vec::new();
    let header = serde_json::to_string(&SessionFileHeader {
        version: SESSION_FORMAT_VERSION,
    })
    .unwrap();
    data.extend_from_slice(header.as_bytes());
    data.push(b'\n');
    for event in [&user, &assistant, &result, &compaction] {
        data.extend_from_slice(serde_json::to_string(event).unwrap().as_bytes());
        data.push(b'\n');
    }
    data.extend_from_slice(br#"{"type":"user_message","content":"tor"#);
    data.push(b'\n');
    (data, superseded_id)
}

/// The core R1 guarantee: ONE traversal of the byte stream yields every
/// resume artifact — the events, the usage rollup, the compaction
/// supersession marks, the action-log rebuild inputs, and the
/// skipped-line summary. The instrumented reader proves each byte of the
/// history was served exactly once.
#[test]
fn single_pass_reader_yields_every_artifact_from_one_traversal() {
    let (data, superseded_id) = replay_fixture();
    let bytes_served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let reader = std::io::BufReader::new(CountingReader {
        inner: std::io::Cursor::new(data.clone()),
        bytes_served: std::sync::Arc::clone(&bytes_served),
    });

    let artifacts = io::read_session_events_from(reader, "single-pass").unwrap();

    assert_eq!(
        bytes_served.load(std::sync::atomic::Ordering::Relaxed),
        data.len(),
        "every byte of the history must be served exactly once — a \
         second traversal would double the count",
    );

    // Events + skip summary.
    assert_eq!(artifacts.events.len(), 4);
    assert_eq!(artifacts.skipped_lines, 1, "the torn line is counted");
    assert_eq!(artifacts.format_version, Some(SESSION_FORMAT_VERSION));

    // Usage rollup matches the reference summation over the same events.
    let reference = sum_usage_from_events(&artifacts.events);
    assert_eq!(artifacts.usage.input_tokens, reference.input_tokens);
    assert_eq!(artifacts.usage.output_tokens, reference.output_tokens);
    assert_eq!(
        artifacts.usage.cache_read_tokens,
        reference.cache_read_tokens
    );
    assert_eq!(artifacts.usage.input_tokens, 11);
    assert_eq!(artifacts.usage.output_tokens, 7);
    assert_eq!(artifacts.usage.cache_read_tokens, 3);

    // Compaction supersession marks.
    assert!(artifacts.superseded_event_ids.contains(&superseded_id));
    assert_eq!(artifacts.superseded_event_ids.len(), 1);

    // Action-log rebuild inputs: the recovered events themselves carry
    // the envelope-bearing tool-call arguments the (single-pass) rebuild
    // consumes.
    let has_call = artifacts.events.iter().any(|event| {
        matches!(
            event,
            SessionEvent::AssistantMessage { tool_calls, .. }
                if tool_calls.iter().any(|tc| tc.call_id == "call_replay_1")
        )
    });
    assert!(has_call, "assistant tool call recovered for the rebuild");
}

/// `ReplayArtifacts::from_events` (the in-memory path used when
/// restoring from a live `EventStore`) derives exactly what the file
/// reader derives for the same history.
#[test]
fn from_events_matches_file_reader_derivations() {
    let (data, superseded_id) = replay_fixture();
    let from_file =
        io::read_session_events_from(std::io::BufReader::new(std::io::Cursor::new(data)), "s")
            .unwrap();
    let from_events = ReplayArtifacts::from_events(from_file.events.clone());

    assert_eq!(from_events.events.len(), from_file.events.len());
    assert_eq!(from_events.usage.input_tokens, from_file.usage.input_tokens);
    assert_eq!(
        from_events.usage.output_tokens,
        from_file.usage.output_tokens
    );
    assert_eq!(
        from_events.usage.cache_read_tokens,
        from_file.usage.cache_read_tokens
    );
    assert_eq!(
        from_events.superseded_event_ids,
        from_file.superseded_event_ids
    );
    assert!(from_events.superseded_event_ids.contains(&superseded_id));
    // File-recovery fields do not apply to the in-memory path.
    assert_eq!(from_events.skipped_lines, 0);
    assert_eq!(from_events.format_version, None);
}

// ----- Index lock acquisition deadline (R2) --------------------------------

/// Holding the lock elsewhere: a deadline-bound acquisition must fail
/// with the typed timeout, and an indefinite (None) acquisition must
/// still be the default behaviour once the lock is free.
#[test]
fn index_lock_deadline_times_out_typed_while_lock_is_held() {
    let tmp = tempfile::tempdir().unwrap();
    let held = super::lock::lock_index(tmp.path(), None).unwrap();

    let deadline = std::time::Duration::from_millis(50);
    let err = super::lock::lock_index(tmp.path(), Some(deadline)).unwrap_err();
    match err {
        SessionPersistError::IndexLockTimeout { path, waited } => {
            assert_eq!(waited, deadline);
            assert!(
                path.ends_with("index.lock"),
                "timeout must name the lock file, got {}",
                path.display(),
            );
        }
        other => panic!("expected IndexLockTimeout, got {other:?}"),
    }

    drop(held);
    // Released: a deadline-bound acquisition now succeeds.
    let _reacquired =
        super::lock::lock_index(tmp.path(), Some(std::time::Duration::from_secs(30))).unwrap();
}

/// The deadline threads through the public read-modify-rewrite path: an
/// `update_index_entry` behind a held lock fails typed with the index
/// untouched, and succeeds after release.
#[test]
fn update_index_entry_respects_lock_deadline_and_leaves_index_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());

    let held = super::lock::lock_index(tmp.path(), None).unwrap();
    let err = update_index_entry(
        tmp.path(),
        &entry.id,
        Some(std::time::Duration::from_millis(50)),
        |e| e.event_count = 99,
    )
    .unwrap_err();
    assert!(
        matches!(err, SessionPersistError::IndexLockTimeout { .. }),
        "expected IndexLockTimeout, got {err:?}",
    );
    drop(held);

    let index = read_index(tmp.path()).unwrap();
    assert_eq!(index[0].event_count, 0, "timed-out update wrote nothing");

    update_index_entry(
        tmp.path(),
        &entry.id,
        Some(std::time::Duration::from_secs(30)),
        |e| e.event_count = 7,
    )
    .unwrap();
    assert_eq!(read_index(tmp.path()).unwrap()[0].event_count, 7);
}

/// The manager applies its configured deadline to every index mutation
/// it performs; `None` (the constructor default) preserves the
/// indefinite wait.
#[test]
fn manager_index_lock_deadline_bounds_create() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path())
        .with_index_lock_deadline(Some(std::time::Duration::from_millis(50)));

    let held = super::lock::lock_index(tmp.path(), None).unwrap();
    let err = manager
        .create(
            CreateSessionOptions {
                model: "gpt".to_owned(),
                working_dir: "/w".to_owned(),
                name: None,
            },
            DurabilityPolicy::Flush,
        )
        .unwrap_err();
    assert!(
        matches!(err, SessionPersistError::IndexLockTimeout { .. }),
        "expected IndexLockTimeout, got {err:?}",
    );
    drop(held);

    manager
        .create(
            CreateSessionOptions {
                model: "gpt".to_owned(),
                working_dir: "/w".to_owned(),
                name: None,
            },
            DurabilityPolicy::Flush,
        )
        .expect("creates normally once the lock is free");
    assert_eq!(read_index(tmp.path()).unwrap().len(), 1);
}

/// Regression: a deadline-bound acquisition must not leak a waiter thread
/// (or its blocked lock descriptor) per timed-out call. The old
/// implementation spawned a `norn-index-lock-wait` thread that parked in
/// the blocking `File::lock` with no cancellation, so it stayed blocked —
/// holding the moved-in lock FD — until the contending holder released;
/// a workflow retrying behind a wedged holder accumulated one thread + FD
/// per timeout without bound. The poll-loop implementation runs entirely
/// on the caller's thread and drops the handle on timeout, leaving nothing
/// to reap.
#[test]
fn index_lock_deadline_does_not_leak_waiter_threads() {
    let tmp = tempfile::tempdir().unwrap();
    // Hold the lock for the whole test so every bounded attempt below
    // times out — exactly the window in which the old waiter thread would
    // stay parked in flock.
    let held = super::lock::lock_index(tmp.path(), None).unwrap();

    let baseline = current_thread_count();
    let attempts = 32;
    for _ in 0..attempts {
        let err = super::lock::lock_index(tmp.path(), Some(std::time::Duration::from_millis(10)))
            .unwrap_err();
        assert!(
            matches!(err, SessionPersistError::IndexLockTimeout { .. }),
            "expected IndexLockTimeout, got {err:?}",
        );
    }

    if let (Some(before), Some(after)) = (baseline, current_thread_count()) {
        assert!(
            after < before + attempts,
            "timed-out acquisitions leaked waiter threads: {before} -> {after} \
             across {attempts} attempts",
        );
    } else {
        // No /proc/self/task on this platform (e.g. macOS): the poll-loop
        // implementation spawns no waiter thread by construction, so there
        // is nothing to leak; the typed-timeout and re-acquisition checks
        // below still run.
        tracing::info!(
            "thread-count assertion skipped: /proc/self/task unavailable on this platform",
        );
    }

    drop(held);
    // Released: a bounded acquisition succeeds again, proving the poll
    // loop acquires the freed lock rather than merely timing out.
    let _reacquired =
        super::lock::lock_index(tmp.path(), Some(std::time::Duration::from_secs(30))).unwrap();
}

/// Best-effort live-thread count via `/proc/self/task` (Linux). Returns
/// `None` where that interface is absent, letting the caller skip the
/// platform-specific assertion at runtime.
fn current_thread_count() -> Option<usize> {
    let entries = std::fs::read_dir("/proc/self/task").ok()?;
    Some(entries.flatten().count())
}

// ----- Concurrent-create header exclusivity (R3) ---------------------------

/// Regression: the first open used check-then-write ("len == 0 → write
/// header"), so two processes racing the first open could BOTH stamp a
/// header line; the tolerant reader then counted the second one as a
/// corrupt skipped line forever. Creation is now `O_EXCL`-style: exactly
/// one opener creates the file and stamps exactly one header.
#[test]
fn concurrent_first_opens_stamp_exactly_one_header() {
    let tmp = tempfile::tempdir().unwrap();
    let path = session_file_path(tmp.path(), "race-header");

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                io::open_session_append(&path).map(|_| ())
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }

    let content = fs::read_to_string(&path).unwrap();
    let header_lines = content
        .lines()
        .filter(|line| serde_json::from_str::<SessionFileHeader>(line).is_ok())
        .count();
    assert_eq!(header_lines, 1, "exactly one header line: {content:?}");
    assert_eq!(content.lines().count(), 1, "nothing but the header");

    let artifacts = io::read_session_events(tmp.path(), "race-header").unwrap();
    assert_eq!(artifacts.skipped_lines, 0, "no corrupt duplicate header");
    assert_eq!(artifacts.format_version, Some(SESSION_FORMAT_VERSION));
}

/// Regression: the create winner used to write its header in a second,
/// non-atomic step after `create_new`, so a winner preempted between the
/// two let a racing loser append its first event ahead of the header —
/// leaving the header permanently skipped at line 2 and `format_version`
/// lost. The header now lands atomically with the file (temp + fsync +
/// `hard_link`), so the very first content line is always the header even
/// when every opener writes an event the instant its handle is returned.
#[test]
fn concurrent_first_opens_keep_the_header_first() {
    let tmp = tempfile::tempdir().unwrap();
    let path = session_file_path(tmp.path(), "header-first");

    let openers = 8;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(openers));
    let handles: Vec<_> = (0..openers)
        .map(|i| {
            let path = path.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let mut file = io::open_session_append(&path).unwrap();
                // Append an event the moment the handle is returned: a
                // loser's first event must never be able to precede the
                // winner's header.
                let mut line = serde_json::to_vec(&user_msg(&format!("event-{i}"))).unwrap();
                line.push(b'\n');
                file.write_all(&line).unwrap();
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }

    let content = fs::read_to_string(&path).unwrap();
    let first = content.lines().next().expect("file has content");
    assert!(
        serde_json::from_str::<SessionFileHeader>(first).is_ok(),
        "the first content line must be the header, got: {first}",
    );
    let header_lines = content
        .lines()
        .filter(|line| serde_json::from_str::<SessionFileHeader>(line).is_ok())
        .count();
    assert_eq!(header_lines, 1, "exactly one header line: {content:?}");

    // The reader stamps the version and recovers every event, with the
    // header never counted as a corrupt/skipped line.
    let artifacts = io::read_session_events(tmp.path(), "header-first").unwrap();
    assert_eq!(artifacts.format_version, Some(SESSION_FORMAT_VERSION));
    assert_eq!(artifacts.events.len(), openers, "all events recovered");
    assert_eq!(
        artifacts.skipped_lines, 0,
        "the header never becomes a skipped corrupt line",
    );
}

/// A pre-existing EMPTY session file (creator crashed before the header
/// landed, or external truncation) is never retro-stamped — writing a
/// header on "observed empty" would reopen the check-then-write race.
/// The file simply loads as pre-versioning format.
#[test]
fn preexisting_empty_file_is_not_retro_stamped_with_header() {
    let tmp = tempfile::tempdir().unwrap();
    let path = session_file_path(tmp.path(), "empty-pre");
    fs::create_dir_all(tmp.path()).unwrap();
    fs::File::create(&path).unwrap();

    let mut sink = JsonlSink::open(&path).unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        0,
        "open must not stamp a header into a pre-existing empty file",
    );
    sink.persist(&user_msg("first")).unwrap();
    drop(sink);

    let artifacts = io::read_session_events(tmp.path(), "empty-pre").unwrap();
    assert_eq!(artifacts.events.len(), 1);
    assert_eq!(artifacts.skipped_lines, 0);
    assert_eq!(
        artifacts.format_version, None,
        "a headerless file reads as pre-versioning format",
    );
}

/// Gap 2 closure: the `Fork` variant was deleted with `SessionTree`. A
/// stray test-era file carrying a persisted `"type":"Fork"` line must
/// degrade gracefully — the tolerant reader skips the unknown variant,
/// counts it, and every other event still loads.
#[test]
fn deleted_fork_variant_line_is_skipped_by_tolerant_reader() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &[user_msg("before")], false).unwrap();

    // Hand-write the exact wire shape the deleted variant used to emit.
    let stray = format!(
        "{{\"type\":\"Fork\",\"base\":{{\"id\":\"{}\",\"parent_id\":null,\
         \"timestamp\":\"{}\"}},\"source_event_id\":\"{}\",\
         \"forked_session_id\":\"dead-child\"}}",
        EventId::new(),
        Utc::now().to_rfc3339(),
        EventId::new(),
    );
    let path = session_file_path(tmp.path(), &entry.id);
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(file, "{stray}").unwrap();
    drop(file);
    append_events(tmp.path(), &entry.id, &[user_msg("after")], false).unwrap();

    let artifacts = io::read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(
        artifacts.skipped_lines, 1,
        "the deleted variant's line must be counted as skipped",
    );
    assert_eq!(
        artifacts.events.len(),
        2,
        "every surrounding event still loads",
    );
}

/// Child index rows (rel_path + parent_id) are additive: legacy rows
/// without the fields deserialize with `None` and keep resolving to the
/// flat path; new rows resolve through rel_path.
#[test]
fn rel_path_rows_resolve_nested_and_legacy_rows_stay_flat() {
    let tmp = tempfile::tempdir().unwrap();
    let legacy = fresh_session(tmp.path());
    assert_eq!(legacy.rel_path, None);
    assert_eq!(
        io::resolved_session_file_path(tmp.path(), &legacy),
        session_file_path(tmp.path(), &legacy.id),
        "legacy rows keep the flat derivation",
    );

    let mut child = legacy.clone();
    child.id = "11111111-2222-4333-8444-555555555555".to_owned();
    child.rel_path = Some(format!("{}/children/fork-1a2b3c4d.jsonl", legacy.id));
    child.parent_id = Some(legacy.id.clone());
    index::insert_child_index_entry(tmp.path(), &child, None).unwrap();
    assert_eq!(
        io::resolved_session_file_path(tmp.path(), &child),
        tmp.path()
            .join(&legacy.id)
            .join("children")
            .join("fork-1a2b3c4d.jsonl"),
    );

    // Round-trip through the index file: the optional fields survive and
    // legacy rows still parse.
    let rows = index::read_index(tmp.path()).unwrap();
    let reread = rows.iter().find(|e| e.id == child.id).unwrap();
    assert_eq!(reread.rel_path, child.rel_path);
    assert_eq!(reread.parent_id.as_deref(), Some(legacy.id.as_str()));
}

// -- Session-fidelity Gap 8: durable context marks in replay ------------

/// A session file written **before** the `ContextMark` variant and the
/// `loop.step_stopped` / `loop.partial_output` records existed must still
/// load in full: raw JSONL lines exactly as an old binary wrote them —
/// no new variants anywhere — parse with zero skips and empty
/// suppressed/injected artifact sets.
#[test]
fn old_format_session_file_without_new_variants_still_loads() {
    let file = concat!(
        r#"{"norn_session_format":1}"#,
        "\n",
        r#"{"type":"UserMessage","base":{"id":"old-u1","parent_id":null,"timestamp":"2026-01-02T03:04:05Z"},"content":"hello"}"#,
        "\n",
        r#"{"type":"AssistantMessage","base":{"id":"old-a1","parent_id":"old-u1","timestamp":"2026-01-02T03:04:06Z"},"content":"hi","thinking":"","tool_calls":[],"usage":{"input_tokens":3,"output_tokens":2,"cache_read_tokens":1,"cache_write_tokens":0}}"#,
        "\n",
        r#"{"type":"Compaction","base":{"id":"old-c1","parent_id":null,"timestamp":"2026-01-02T03:04:07Z"},"summary":"old summary","replaced_event_ids":["old-u1"]}"#,
        "\n",
        r#"{"type":"Custom","base":{"id":"old-x1","parent_id":null,"timestamp":"2026-01-02T03:04:08Z"},"event_type":"loop.truncated","data":{"stop_reason":"max_tokens"}}"#,
        "\n",
    );
    let artifacts = io::read_session_events_from(
        std::io::BufReader::new(std::io::Cursor::new(file.as_bytes().to_vec())),
        "old-format",
    )
    .unwrap();

    assert_eq!(artifacts.skipped_lines, 0, "every old line must parse");
    assert_eq!(artifacts.events.len(), 4);
    let superseded: EventId = "old-u1".parse().unwrap();
    assert!(artifacts.superseded_event_ids.contains(&superseded));
    assert!(
        artifacts.suppressed_event_ids.is_empty(),
        "an old file carries no suppress marks",
    );
    assert!(
        artifacts.injected_event_ids.is_empty(),
        "an old file carries no inject marks",
    );
    assert_eq!(artifacts.usage.input_tokens, 3);
    assert_eq!(artifacts.usage.output_tokens, 2);
}

/// Persisted `ContextMark` lines rebuild the suppressed and injected
/// artifact sets — the file-reader half of the Gap 8 resume path — and
/// the in-memory `from_events` path derives the identical sets.
#[test]
fn context_mark_lines_rebuild_suppress_and_inject_sets() {
    let file = concat!(
        r#"{"norn_session_format":1}"#,
        "\n",
        r#"{"type":"UserMessage","base":{"id":"m-u1","parent_id":null,"timestamp":"2026-07-06T00:00:01Z"},"content":"keep"}"#,
        "\n",
        r#"{"type":"UserMessage","base":{"id":"m-u2","parent_id":null,"timestamp":"2026-07-06T00:00:02Z"},"content":"noisy"}"#,
        "\n",
        r#"{"type":"ContextMark","base":{"id":"m-s1","parent_id":null,"timestamp":"2026-07-06T00:00:03Z"},"mark":"suppress","target_event_id":"m-u2"}"#,
        "\n",
        r#"{"type":"UserMessage","base":{"id":"m-u3","parent_id":null,"timestamp":"2026-07-06T00:00:04Z"},"content":"injected note"}"#,
        "\n",
        r#"{"type":"ContextMark","base":{"id":"m-i1","parent_id":null,"timestamp":"2026-07-06T00:00:05Z"},"mark":"inject","target_event_id":"m-u3"}"#,
        "\n",
    );
    let artifacts = io::read_session_events_from(
        std::io::BufReader::new(std::io::Cursor::new(file.as_bytes().to_vec())),
        "context-marks",
    )
    .unwrap();

    assert_eq!(artifacts.skipped_lines, 0);
    assert_eq!(artifacts.events.len(), 5);
    let suppressed: EventId = "m-u2".parse().unwrap();
    let injected: EventId = "m-u3".parse().unwrap();
    assert_eq!(artifacts.suppressed_event_ids.len(), 1);
    assert!(artifacts.suppressed_event_ids.contains(&suppressed));
    assert_eq!(artifacts.injected_event_ids.len(), 1);
    assert!(artifacts.injected_event_ids.contains(&injected));

    let from_events = ReplayArtifacts::from_events(artifacts.events.clone());
    assert_eq!(
        from_events.suppressed_event_ids,
        artifacts.suppressed_event_ids
    );
    assert_eq!(from_events.injected_event_ids, artifacts.injected_event_ids);
}
