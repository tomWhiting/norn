#[test]
fn create_returns_indexed_sink_registered_store() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let opened = manager
        .create(options("gpt-x"), DurabilityPolicy::Flush)
        .unwrap();
    assert_eq!(opened.replay, ReplaySummary::default());
    assert_eq!(opened.entry.model, "gpt-x");
    assert_eq!(opened.entry.format_version, SESSION_FORMAT_VERSION);

    // Indexed immediately.
    let listed = manager.list().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, opened.entry.id);

    // The store writes through and the registered sink maintains
    // the index at checkpoint.
    opened.store.append(user_msg("hello")).unwrap();
    opened.store.append(assistant_with_usage(7, 3, 1)).unwrap();
    opened.store.checkpoint().unwrap();
    let read = read_session_events(tmp.path(), &opened.entry.id).unwrap();
    assert_eq!(read.events.len(), 2);
    let listed = manager.list().unwrap();
    assert_eq!(listed[0].event_count, 2);
    assert_eq!(listed[0].total_input_tokens, 7);
    assert_eq!(listed[0].total_output_tokens, 3);
    assert_eq!(listed[0].total_cache_read_tokens, 1);
}

#[test]
fn create_honors_name_and_resolve_finds_it() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let opened = manager
        .create(
            CreateSessionOptions {
                model: "gpt".to_owned(),
                working_dir: "/w".to_owned(),
                name: Some("nightly".to_owned()),
            },
            DurabilityPolicy::Flush,
        )
        .unwrap();
    let resolved = manager.resolve("nightly").unwrap();
    assert_eq!(resolved.id, opened.entry.id);
}

/// `create_with_id`: the caller's exact ID names the session and its
/// file; a second create with the same ID fails typed (never
/// attaching to the first session's history); validation matches the
/// `open_or_resume` rules.
#[test]
fn create_with_id_uses_exact_id_and_refuses_collision() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let opened = manager
        .create_with_id("wf-build-1234", options("gpt"), DurabilityPolicy::Flush)
        .unwrap();
    assert_eq!(opened.entry.id, "wf-build-1234");
    opened.store.append(user_msg("step one")).unwrap();
    drop(opened);

    // Resumable by the caller-chosen ID.
    let resumed = manager
        .resume("wf-build-1234", DurabilityPolicy::Flush)
        .unwrap();
    assert_eq!(resumed.replay.replayed_events, 1);
    drop(resumed);

    // Create-exactly-this: the collision is a typed refusal and the
    // existing session's history is untouched.
    let err = manager
        .create_with_id("wf-build-1234", options("gpt"), DurabilityPolicy::Flush)
        .expect_err("an existing id must fail loudly");
    assert!(
        matches!(&err, SessionPersistError::IdExists { id } if id == "wf-build-1234"),
        "expected IdExists, got {err:?}",
    );
    let read = read_session_events(tmp.path(), "wf-build-1234").unwrap();
    assert_eq!(read.events.len(), 1, "prior history untouched");

    let invalid = manager
        .create_with_id("../escape", options("gpt"), DurabilityPolicy::Flush)
        .expect_err("path-capable ids are rejected");
    assert!(
        matches!(invalid, SessionPersistError::InvalidSessionId { .. }),
        "expected InvalidSessionId, got {invalid:?}",
    );
}

/// An orphan `{id}.jsonl` with no index makes the strict store ambiguous.
/// Creation must fail closed without replacing either authority candidate.
#[test]
fn create_with_id_refuses_orphan_session_file() {
    use std::io::Write as _;
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());

    let orphan_path = tmp.path().join("wf-restored-7.jsonl");
    let mut file = std::fs::File::create(&orphan_path).unwrap();
    writeln!(file, "{{\"format_version\":1}}").unwrap();
    drop(file);

    let err = manager
        .create_with_id("wf-restored-7", options("gpt"), DurabilityPolicy::Flush)
        .expect_err("an orphan session file must refuse the create");
    assert!(
        matches!(&err, SessionPersistError::MissingIndex { path } if path == tmp.path()),
        "expected MissingIndex, got {err:?}",
    );
    assert!(
        !index_file_path(tmp.path()).exists(),
        "the refusal must not have created an index",
    );
    let on_disk = std::fs::read_to_string(&orphan_path).unwrap();
    assert_eq!(
        on_disk.lines().count(),
        1,
        "the orphan file must be untouched",
    );
}

#[test]
fn resume_replays_events_with_clean_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let opened = manager
        .create(options("gpt"), DurabilityPolicy::Flush)
        .unwrap();
    let id = opened.entry.id.clone();
    opened.store.append(user_msg("one")).unwrap();
    opened.store.append(user_msg("two")).unwrap();
    drop(opened);

    let resumed = manager.resume(&id, DurabilityPolicy::Flush).unwrap();
    assert_eq!(resumed.replay.replayed_events, 2);
    assert_eq!(resumed.store.len(), 2);
    assert_eq!(resumed.entry.id, id);

    // Continued appends land in the same file.
    resumed.store.append(user_msg("three")).unwrap();
    drop(resumed);
    let read = read_session_events(tmp.path(), &id).unwrap();
    assert_eq!(read.events.len(), 3);
}

#[test]
fn resume_latest_in_working_dir_ignores_global_latest_elsewhere() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let current = manager
        .create(options_in("gpt", "/repo/current"), DurabilityPolicy::Flush)
        .unwrap();
    let current_id = current.entry.id.clone();
    drop(current);

    std::thread::sleep(std::time::Duration::from_millis(5));
    let other = manager
        .create(options_in("gpt", "/repo/other"), DurabilityPolicy::Flush)
        .unwrap();
    let other_id = other.entry.id.clone();
    drop(other);

    let resumed = manager
        .resume_latest_in_working_dir("/repo/current", DurabilityPolicy::Flush)
        .unwrap();
    assert_eq!(
        resumed.entry.id, current_id,
        "must not resume globally newer session {other_id} from another working directory",
    );
}

#[test]
fn fork_latest_in_working_dir_uses_current_directory_source() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let current = manager
        .create(options_in("gpt", "/repo/current"), DurabilityPolicy::Flush)
        .unwrap();
    let current_id = current.entry.id.clone();
    current.store.append(user_msg("current source")).unwrap();
    drop(current);

    std::thread::sleep(std::time::Duration::from_millis(5));
    let other = manager
        .create(options_in("gpt", "/repo/other"), DurabilityPolicy::Flush)
        .unwrap();
    other.store.append(user_msg("other source")).unwrap();
    drop(other);

    let fork = manager
        .fork_latest_in_working_dir(
            "/repo/current",
            options_in("gpt", "/repo/current"),
            DurabilityPolicy::Flush,
        )
        .unwrap();
    assert_ne!(fork.entry.id, current_id, "fork creates a new session");
    let events = fork.store.events();
    assert_eq!(events.len(), 2, "source event plus fork marker");
    assert!(
        matches!(
            &events[0],
            SessionEvent::UserMessage { content, .. } if content == "current source"
        ),
        "fork must copy the current-directory source session",
    );
}

/// An EOF-incomplete final row is removed only after the retained strict
/// prefix validates, so the summary reports exactly the retained events.
#[test]
fn resume_recovers_incomplete_final_row_after_strict_prefix() {
    use std::io::Write as _;
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let opened = manager
        .create(options("gpt"), DurabilityPolicy::Flush)
        .unwrap();
    let id = opened.entry.id.clone();
    opened.store.append(user_msg("intact")).unwrap();
    drop(opened);

    // Tear the file the way ENOSPC / `kill -9` would.
    let path = session_file_path(tmp.path(), &id);
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(br#"{"type":"user_message","content":"tor"#)
        .unwrap();
    drop(file);

    let resumed = manager.resume(&id, DurabilityPolicy::Flush).unwrap();
    assert_eq!(resumed.replay.replayed_events, 1);
}

#[test]
fn resume_self_heals_drifted_index_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let opened = manager
        .create(options("gpt"), DurabilityPolicy::Flush)
        .unwrap();
    let id = opened.entry.id.clone();
    opened.store.append(user_msg("one")).unwrap();
    opened.store.append(assistant_with_usage(10, 5, 2)).unwrap();
    opened.store.checkpoint().unwrap();
    drop(opened);

    // Simulate crash staleness: zero the entry behind the manager's
    // back.
    update_index_entry(tmp.path(), &id, None, |e| {
        e.event_count = 0;
        e.total_input_tokens = 0;
        e.total_output_tokens = 0;
        e.total_cache_read_tokens = 0;
    })
    .unwrap();

    let resumed = manager.resume(&id, DurabilityPolicy::Flush).unwrap();
    assert_eq!(resumed.entry.event_count, 2);
    assert_eq!(resumed.entry.total_input_tokens, 10);
    assert_eq!(resumed.entry.total_output_tokens, 5);
    assert_eq!(resumed.entry.total_cache_read_tokens, 2);
    let listed = manager.list().unwrap();
    assert_eq!(listed[0].event_count, 2, "repair persisted to disk");
}

#[test]
fn fork_copies_events_appends_marker_and_attaches_sink() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let source = manager
        .create(options("gpt"), DurabilityPolicy::Flush)
        .unwrap();
    let source_id = source.entry.id.clone();
    source.store.append(user_msg("one")).unwrap();
    source.store.append(user_msg("two")).unwrap();
    let last_id = source.store.last_event_id().unwrap();
    drop(source);

    let fork = manager
        .fork(&source_id, options("gpt-fork"), DurabilityPolicy::Flush)
        .unwrap();
    assert_ne!(fork.entry.id, source_id);
    assert_eq!(fork.entry.model, "gpt-fork");
    assert_eq!(fork.replay.replayed_events, 3, "2 copied + branch marker");
    assert_eq!(fork.store.len(), 3);
    assert_eq!(
        fork.entry.event_count, 3,
        "returned entry reflects the batch append",
    );
    match fork.store.events().last().unwrap() {
        SessionEvent::ChildBranch {
            parent_session_id,
            child_session_id,
            path_address,
            parent_event_anchor,
            kind,
            ..
        } => {
            assert_eq!(parent_session_id.as_deref(), Some(source_id.as_str()));
            assert_eq!(child_session_id.as_deref(), Some(fork.entry.id.as_str()));
            assert_eq!(path_address, ROOT_PATH_ADDRESS);
            assert_eq!(parent_event_anchor.as_ref(), Some(&last_id));
            assert_eq!(*kind, ChildBranchKind::Fork);
        }
        other => panic!("expected ChildBranch tail, got {other:?}"),
    }

    // The fork's sink is live: an append after forking persists.
    let fork_id = fork.entry.id.clone();
    fork.store.append(user_msg("post-fork")).unwrap();
    drop(fork);
    let read = read_session_events(tmp.path(), &fork_id).unwrap();
    assert_eq!(read.events.len(), 4);

    // Source file untouched.
    let source_read = read_session_events(tmp.path(), &source_id).unwrap();
    assert_eq!(source_read.events.len(), 2);
}

#[test]
fn fork_empty_source_returns_empty_source() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let source = manager
        .create(options("gpt"), DurabilityPolicy::Flush)
        .unwrap();
    let err = manager
        .fork(&source.entry.id, options("gpt"), DurabilityPolicy::Flush)
        .unwrap_err();
    assert!(matches!(err, SessionPersistError::EmptySource { .. }));
}
