type OpenIdTestResult<T = ()> =
    Result<T, Box<dyn std::error::Error + Send + Sync + 'static>>;

fn open_id_expected_error<T>(
    result: Result<T, SessionPersistError>,
    context: &str,
) -> Result<SessionPersistError, std::io::Error> {
    match result {
        Ok(_) => Err(std::io::Error::other(format!(
            "{context} unexpectedly succeeded"
        ))),
        Err(error) => Ok(error),
    }
}

fn open_id_panic_detail(payload: &(dyn std::any::Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message
    } else {
        match payload.downcast_ref::<String>() {
            Some(message) => message.as_str(),
            None => "non-string panic payload",
        }
    }
}

#[test]
fn open_or_resume_creates_with_caller_supplied_id() -> OpenIdTestResult {
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    let opened = manager
        .open_or_resume("wf-1234.step-2", options("gpt"), DurabilityPolicy::Flush)
        ?;
    assert_eq!(opened.entry.id, "wf-1234.step-2");
    assert_eq!(opened.replay, ReplaySummary::default());
    opened.store.append(user_msg("first attempt"))?;
    drop(opened);

    let listed = manager.list()?;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "wf-1234.step-2");
    Ok(())
}

/// The idempotency contract: a retry with the same deterministic key
/// resumes the prior attempt's session — same ID, same history, one
/// index entry — instead of minting a new session per attempt.
#[test]
fn open_or_resume_retry_resumes_prior_session() -> OpenIdTestResult {
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    let first = manager
        .open_or_resume("wf-77.activity-3", options("gpt"), DurabilityPolicy::Flush)
        ?;
    first.store.append(user_msg("attempt one"))?;
    drop(first);

    let retry = manager
        .open_or_resume("wf-77.activity-3", options("gpt"), DurabilityPolicy::Flush)
        ?;
    assert_eq!(retry.entry.id, "wf-77.activity-3");
    assert_eq!(retry.replay.replayed_events, 1);
    assert_eq!(retry.store.len(), 1, "prior history replayed");
    drop(retry);

    assert_eq!(manager.list()?.len(), 1, "no duplicate entry");
    Ok(())
}

/// An indexed row without its timeline cannot be produced by the journaled
/// format-2 publisher. Refuse it rather than silently adopting an incomplete
/// or externally constructed session.
#[test]
fn open_or_resume_refuses_entry_without_timeline() -> OpenIdTestResult {
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    // Construct the invalid state directly, bypassing normal publication.
    let entry = new_index_entry("wf-crash".to_owned(), options("gpt"));
    append_index_entry(tmp.path(), &entry, None)?;
    assert!(!session_file_path(tmp.path(), "wf-crash").exists());

    let error = open_id_expected_error(
        manager.open_or_resume("wf-crash", options("gpt"), DurabilityPolicy::Flush),
        "opening an indexed entry without its timeline",
    )?;
    assert!(
        matches!(&error, SessionPersistError::NotFound { input } if input == "wf-crash"),
        "expected NotFound for the missing registered timeline, got {error:?}",
    );
    assert!(!session_file_path(tmp.path(), "wf-crash").exists());
    assert_eq!(manager.list()?.len(), 1);
    Ok(())
}

#[test]
fn open_or_resume_matches_exact_id_never_name_or_prefix() -> OpenIdTestResult {
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    // A session *named* "alpha" with a random UUID id.
    manager
        .create(
            CreateSessionOptions {
                model: "gpt".to_owned(),
                working_dir: "/w".to_owned(),
                name: Some("alpha".to_owned()),
            },
            DurabilityPolicy::Flush,
        )
        ?;

    // open_or_resume("alpha") must NOT attach to the named session —
    // it creates a new one whose ID is literally "alpha".
    let opened = manager
        .open_or_resume("alpha", options("gpt"), DurabilityPolicy::Flush)
        ?;
    assert_eq!(opened.entry.id, "alpha");
    assert_eq!(opened.replay.replayed_events, 0);
    assert_eq!(manager.list()?.len(), 2);
    Ok(())
}

#[test]
fn open_or_resume_rejects_path_capable_ids() -> OpenIdTestResult {
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    for bad in [
        "",
        ".",
        "..",
        "../evil",
        "a/b",
        "a\\b",
        ".hidden",
        "-rf",
        "id with space",
        "id:colon",
    ] {
        let err = open_id_expected_error(
            manager.open_or_resume(bad, options("gpt"), DurabilityPolicy::Flush),
            "opening a path-capable session id",
        )?;
        assert!(
            matches!(err, SessionPersistError::InvalidSessionId { .. }),
            "id {bad:?} must be rejected, got {err:?}",
        );
    }
    assert!(
        manager.list()?.is_empty(),
        "rejected ids must leave no index entries",
    );
    Ok(())
}

/// Blocker regression: session IDs map to `{id}.jsonl`, so the id
/// `"index"` mapped onto `{data_dir}/index.jsonl` — the shared session
/// index. `open_or_resume("index", ...)` appended session events into
/// the index and `delete("index")` destroyed it for every session.
/// The whole reserved name family must be rejected at validation.
#[test]
fn open_or_resume_rejects_reserved_persistence_ids() -> OpenIdTestResult {
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    // A real session first, so the index file exists and corruption
    // would be observable.
    let existing = manager
        .create(options("gpt"), DurabilityPolicy::Flush)
        ?;
    let existing_id = existing.entry.id.clone();
    drop(existing);

    for reserved in ["index", "index.lock", "index.jsonl", "index.jsonl.tmp.0"] {
        let err = open_id_expected_error(
            manager.open_or_resume(reserved, options("gpt"), DurabilityPolicy::Flush),
            "opening a reserved persistence id",
        )?;
        assert!(
            matches!(err, SessionPersistError::InvalidSessionId { .. }),
            "reserved id {reserved:?} must be rejected, got {err:?}",
        );
    }

    // Near-misses outside the dotted family stay valid.
    let opened = manager
        .open_or_resume("indexer-1", options("gpt"), DurabilityPolicy::Flush)
        ?;
    assert_eq!(opened.entry.id, "indexer-1");
    drop(opened);

    // The index itself was never written to as a session file: both
    // legitimate sessions are still listed, nothing else.
    let mut ids: Vec<String> = manager.list()?.into_iter().map(|e| e.id).collect();
    ids.sort();
    let mut expected = vec![existing_id, "indexer-1".to_owned()];
    expected.sort();
    assert_eq!(ids, expected);
    Ok(())
}

/// `delete("index")` must never be able to remove the session index.
#[test]
fn delete_can_never_destroy_the_index() -> OpenIdTestResult {
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    let opened = manager
        .create(options("gpt"), DurabilityPolicy::Flush)
        ?;
    drop(opened);

    let err = open_id_expected_error(manager.delete("index"), "deleting the session index")?;
    assert!(
        !matches!(err, SessionPersistError::Io(_)),
        "delete(\"index\") must fail by rejection, not by touching files: {err:?}",
    );
    assert!(
        index_file_path(tmp.path()).exists(),
        "the session index file must survive",
    );
    assert_eq!(manager.list()?.len(), 1, "the index is intact");
    Ok(())
}

/// Defense in depth: a reserved ID can only enter the index through a
/// hand-edited file (every programmatic insertion path rejects it).
/// The strict index reader rejects the entire ambiguous authority rather than
/// routing any operation through a partially accepted index.
#[test]
fn reserved_id_smuggled_into_index_invalidates_authority() -> OpenIdTestResult {
    use std::io::Write as _;
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    let opened = manager
        .create(options("gpt"), DurabilityPolicy::Flush)
        ?;
    drop(opened);

    // Bypass every guard: write the index line by hand.
    let smuggled = new_index_entry("index".to_owned(), options("gpt"));
    let line = serde_json::to_string(&smuggled)?;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(index_file_path(tmp.path()))?;
    writeln!(file, "{line}")?;
    drop(file);

    for (what, err) in [
        (
            "resolve",
            open_id_expected_error(manager.resolve("index"), "resolving a reserved id")?,
        ),
        (
            "resume",
            open_id_expected_error(
                manager.resume("index", DurabilityPolicy::Flush),
                "resuming a reserved id",
            )?,
        ),
        (
            "delete",
            open_id_expected_error(manager.delete("index"), "deleting a reserved id")?,
        ),
        (
            "read_events",
            open_id_expected_error(manager.read_events("index"), "reading a reserved id")?,
        ),
    ] {
        assert!(
            matches!(err, SessionPersistError::InvalidIndex(_)),
            "{what}(\"index\") must fail closed on the unsafe index row, got {err:?}",
        );
    }
    assert!(
        index_file_path(tmp.path()).exists(),
        "the index file must survive every attempt",
    );
    Ok(())
}

/// Two callers racing the same deterministic key (the multi-process
/// topology, simulated with threads — the index lock excludes both)
/// must converge on exactly one session.
#[test]
fn open_or_resume_concurrent_same_id_converges_on_one_session() -> OpenIdTestResult {
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path().to_path_buf();
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let dir = dir.clone();
            std::thread::spawn(move || -> OpenIdTestResult<String> {
                let manager = SessionManager::new(dir);
                let opened = manager
                    .open_or_resume(
                        "wf-race.key",
                        CreateSessionOptions {
                            model: "gpt".to_owned(),
                            working_dir: "/w".to_owned(),
                            name: None,
                        },
                        DurabilityPolicy::Flush,
                    )
                    ?;
                opened
                    .store
                    .append(SessionEvent::UserMessage {
                        base: EventBase::new(None),
                        content: format!("from-{i}"),
                    })
                    ?;
                Ok(opened.entry.id)
            })
        })
        .collect();
    for handle in handles {
        let worker_result = handle.join().map_err(|payload| {
            std::io::Error::other(format!(
                "open-or-resume worker panicked: {}",
                open_id_panic_detail(payload.as_ref()),
            ))
        })?;
        assert_eq!(worker_result?, "wf-race.key");
    }
    let manager = SessionManager::new(tmp.path());
    assert_eq!(manager.list()?.len(), 1, "one entry, no dupes");
    let read = read_session_events(tmp.path(), "wf-race.key")?;
    assert_eq!(read.events.len(), 4, "every caller's append landed");
    Ok(())
}
