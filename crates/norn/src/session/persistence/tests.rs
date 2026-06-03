//! Tests for the session persistence layer (NC-002 R2--R6).

use std::fs;
use std::path::Path;

use crate::provider::usage::Usage;
use crate::session::events::{EventBase, EventId, EventUsage, SessionEvent, ToolCallEvent};
use crate::session::store::EventStore;
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
        SessionEvent::Fork {
            base: EventBase::new(Some(parent.clone())),
            source_event_id: parent,
            forked_session_id: "child".to_owned(),
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

fn fresh_session(dir: &Path) -> SessionIndexEntry {
    create_session(dir, "gpt-x".to_owned(), "/work".to_owned(), None).unwrap()
}

// ----- R2: JSONL serialization -----

#[test]
fn round_trip_all_nine_variants() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let events = one_of_each();
    append_events(tmp.path(), &entry.id, &events, false).unwrap();

    let round_trip = read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(round_trip.len(), events.len());
    for (a, b) in events.iter().zip(round_trip.iter()) {
        assert_event_eq(a, b);
    }
}

#[test]
fn round_trip_at_least_five_variants() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let events: Vec<_> = one_of_each().into_iter().take(5).collect();
    append_events(tmp.path(), &entry.id, &events, false).unwrap();
    let round_trip = read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(round_trip.len(), 5);
    for (a, b) in events.iter().zip(round_trip.iter()) {
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
    let line_count = content.lines().count();
    assert_eq!(line_count, one_of_each().len());
}

#[test]
fn corrupted_line_reports_line_number() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let head: Vec<_> = one_of_each().into_iter().take(2).collect();
    append_events(tmp.path(), &entry.id, &head, false).unwrap();
    let path = session_file_path(tmp.path(), &entry.id);
    let mut existing = fs::read_to_string(&path).unwrap();
    existing.push_str("not-json\n");
    fs::write(&path, existing).unwrap();

    let err = read_session_events(tmp.path(), &entry.id).unwrap_err();
    match err {
        SessionPersistError::Parse { line, .. } => assert_eq!(line, 3),
        other => panic!("expected Parse, got {other:?}"),
    }
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
    let events = read_session_events(tmp.path(), &entry.id).unwrap();
    assert_eq!(events.len(), 1);
}

#[test]
fn empty_file_returns_empty_vec() {
    let tmp = tempfile::tempdir().unwrap();
    let path = session_file_path(tmp.path(), "missing");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "").unwrap();
    let events = read_session_events(tmp.path(), "missing").unwrap();
    assert!(events.is_empty());
}

#[test]
fn missing_file_returns_empty_vec() {
    let tmp = tempfile::tempdir().unwrap();
    let events = read_session_events(tmp.path(), "does-not-exist").unwrap();
    assert!(events.is_empty());
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
fn tmp_file_removed_after_successful_atomic_write() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &one_of_each(), false).unwrap();
    assert!(!index_tmp_path(tmp.path()).exists());
}

#[test]
fn stray_tmp_file_does_not_corrupt_canonical_index() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let canonical_before = fs::read(index_file_path(tmp.path())).unwrap();
    // Drop a bogus .tmp file to mimic a previous crash mid-write.
    fs::write(index_tmp_path(tmp.path()), "garbage\n").unwrap();
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
    assert_eq!(line_count, 6);
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
    let entry = create_session(&nested, "gpt-x".to_owned(), "/work".to_owned(), None).unwrap();
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
}

// ----- R5: resume -----

#[test]
fn resume_reconstructs_event_store() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let events: Vec<_> = one_of_each().into_iter().take(5).collect();
    append_events(tmp.path(), &entry.id, &events, false).unwrap();

    let (store, replayed, resolved) = resume_session(tmp.path(), &entry.id).unwrap();
    assert_eq!(store.len(), 5);
    assert_eq!(replayed.len(), 5);
    assert_eq!(resolved.id, entry.id);
    let store_events = store.events();
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

    let (_, _, resolved) = resume_session(tmp.path(), "").unwrap();
    assert_eq!(
        resolved.id, a.id,
        "expected `a` (most recently updated), not `b={}`",
        b.id
    );
}

#[test]
fn resume_eight_char_unique_prefix_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &[user_msg("hi")], false).unwrap();
    let prefix = &entry.id[..8];
    let (_, _, resolved) = resume_session(tmp.path(), prefix).unwrap();
    assert_eq!(resolved.id, entry.id);
}

#[test]
fn resume_unknown_prefix_returns_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = fresh_session(tmp.path());
    let err = resume_session(tmp.path(), "ffffffff-no-match").unwrap_err();
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
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_read_tokens: 0,
        });
    }
    write_index_atomic(tmp.path(), &entries).unwrap();
    let err = resume_session(tmp.path(), shared_prefix).unwrap_err();
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
    let err = resume_session(tmp.path(), &entry.id[..7]).unwrap_err();
    assert!(matches!(err, SessionPersistError::NotFound { .. }));
}

#[test]
fn resume_by_name_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = create_session(
        tmp.path(),
        "gpt".to_owned(),
        "/w".to_owned(),
        Some("nightly".to_owned()),
    )
    .unwrap();
    append_events(tmp.path(), &entry.id, &[user_msg("hi")], false).unwrap();
    let (_, _, resolved) = resume_session(tmp.path(), "nightly").unwrap();
    assert_eq!(resolved.id, entry.id);
}

#[test]
fn resume_empty_index_returns_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let err = resume_session(tmp.path(), "").unwrap_err();
    assert!(matches!(err, SessionPersistError::NotFound { .. }));
}

// ----- R6: fork -----

#[test]
fn fork_appends_fork_event_at_tail() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let events: Vec<_> = one_of_each().into_iter().take(5).collect();
    append_events(tmp.path(), &entry.id, &events, false).unwrap();

    let (new_entry, _, all_events) =
        fork_session(tmp.path(), &entry.id, "gpt".to_owned(), "/w".to_owned()).unwrap();
    assert_eq!(all_events.len(), 6);

    let body = fs::read_to_string(session_file_path(tmp.path(), &new_entry.id)).unwrap();
    let line_count = body.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(line_count, 6);
}

#[test]
fn fork_event_source_id_matches_last_original() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let events: Vec<_> = one_of_each().into_iter().take(3).collect();
    append_events(tmp.path(), &entry.id, &events, false).unwrap();
    let last_id = events.last().unwrap().base().id.clone();

    let (new_entry, _, all_events) =
        fork_session(tmp.path(), &entry.id, "gpt".to_owned(), "/w".to_owned()).unwrap();
    let fork = all_events.last().unwrap();
    match fork {
        SessionEvent::Fork {
            source_event_id,
            forked_session_id,
            ..
        } => {
            assert_eq!(source_event_id, &last_id);
            assert_eq!(forked_session_id, &new_entry.id);
        }
        other => panic!("expected Fork tail, got {other:?}"),
    }
}

#[test]
fn fork_does_not_modify_source_file() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &one_of_each(), false).unwrap();
    let source_path = session_file_path(tmp.path(), &entry.id);
    let before = fs::read(&source_path).unwrap();
    let _ = fork_session(tmp.path(), &entry.id, "gpt".to_owned(), "/w".to_owned()).unwrap();
    let after = fs::read(&source_path).unwrap();
    assert_eq!(before, after);
}

#[test]
fn fork_index_contains_both_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    append_events(tmp.path(), &entry.id, &one_of_each(), false).unwrap();
    let (new_entry, _, _) =
        fork_session(tmp.path(), &entry.id, "gpt".to_owned(), "/w".to_owned()).unwrap();
    let ids: Vec<String> = read_index(tmp.path())
        .unwrap()
        .into_iter()
        .map(|e| e.id)
        .collect();
    assert!(ids.contains(&entry.id));
    assert!(ids.contains(&new_entry.id));
}

#[test]
fn fork_empty_source_returns_empty_source() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    let err = fork_session(tmp.path(), &entry.id, "gpt".to_owned(), "/w".to_owned()).unwrap_err();
    assert!(matches!(err, SessionPersistError::EmptySource { .. }));
}

#[test]
fn fork_no_argument_resolves_latest() {
    let tmp = tempfile::tempdir().unwrap();
    let _older = fresh_session(tmp.path());
    std::thread::sleep(std::time::Duration::from_millis(5));
    let newer = fresh_session(tmp.path());
    append_events(tmp.path(), &newer.id, &one_of_each(), false).unwrap();
    let (new_entry, _, all_events) =
        fork_session(tmp.path(), "", "gpt".to_owned(), "/w".to_owned()).unwrap();
    assert_eq!(all_events.len(), one_of_each().len() + 1);
    // The forked session is a new entry -- not the newer source.
    assert_ne!(new_entry.id, newer.id);
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
    update_session_index(tmp.path(), &entry.id, 3, &usage).unwrap();

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
    let entry = fresh_session(tmp.path());
    update_session_index(tmp.path(), &entry.id, 4, &Usage::default()).unwrap();
    // The index-only path must never create or touch the session JSONL.
    assert!(!session_file_path(tmp.path(), &entry.id).exists());
}

#[test]
fn update_session_index_accumulates_across_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());
    update_session_index(tmp.path(), &entry.id, 2, &assistant_usage(7, 3, 1)).unwrap();
    update_session_index(tmp.path(), &entry.id, 5, &assistant_usage(4, 6, 2)).unwrap();

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
    update_session_index(tmp.path(), &entry.id, 0, &Usage::default()).unwrap();
    let index = read_index(tmp.path()).unwrap();
    assert_eq!(index[0].event_count, 0);
    assert_eq!(index[0].updated_at, created_updated_at);
}

#[test]
fn update_session_index_unknown_session_is_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let _ = fresh_session(tmp.path());
    let err = update_session_index(tmp.path(), "ghost", 1, &assistant_usage(1, 0, 0)).unwrap_err();
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

/// End-to-end regression for the double-write bug: a write-through
/// `JsonlSink` plus a post-turn `update_session_index` must leave the
/// session JSONL at a 1:1 line-to-event ratio (not 2:1) and remain
/// resumable. Using `append_events` here instead would double-write and
/// trip the duplicate-ID guard in `resume_session`.
#[test]
fn sink_plus_update_index_stays_one_to_one_and_resumable() {
    let tmp = tempfile::tempdir().unwrap();
    let entry = fresh_session(tmp.path());

    // Simulate a turn: write events through the sink (write-through).
    let store = attach_sink(EventStore::new(), tmp.path(), &entry.id);
    let turn = vec![user_msg("hello"), assistant_with_usage(12, 8, 4)];
    for event in &turn {
        store.append(event.clone()).unwrap();
    }

    // Post-turn index reconcile — the fix path.
    let new_events = store.events();
    let appended = u64::try_from(new_events.len()).unwrap();
    let usage = sum_usage_from_events(&new_events);
    update_session_index(tmp.path(), &entry.id, appended, &usage).unwrap();

    // JSONL holds exactly the turn's events — one line each, no duplicates.
    let body = fs::read_to_string(session_file_path(tmp.path(), &entry.id)).unwrap();
    let line_count = body.lines().filter(|l| !l.trim().is_empty()).count();
    assert_eq!(
        line_count,
        turn.len(),
        "expected 1:1 lines, got double-write"
    );

    // Index reflects the turn.
    let index = read_index(tmp.path()).unwrap();
    assert_eq!(index[0].event_count, 2);
    assert_eq!(index[0].total_input_tokens, 12);
    assert_eq!(index[0].total_output_tokens, 8);
    assert_eq!(index[0].total_cache_read_tokens, 4);

    // The session resumes cleanly — the duplicate-ID guard never fires.
    let (resumed, replayed, _) = resume_session(tmp.path(), &entry.id).unwrap();
    assert_eq!(resumed.len(), 2);
    assert_eq!(replayed.len(), 2);
}
