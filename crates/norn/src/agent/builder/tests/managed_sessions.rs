use super::*;

fn manager_in(dir: &std::path::Path) -> SessionManager {
    SessionManager::new(dir)
}

/// `open_session(OpenOrResume)` with the same deterministic id
/// resumes the prior run's history — the retry-safe activity path.
#[tokio::test]
async fn open_session_open_or_resume_continues_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    let sessions = tempfile::tempdir().expect("session dir");
    let manager = manager_in(sessions.path());
    let spec = || SessionSpec::open_or_resume("wf-7.step-2");

    let first = AgentBuilder::new(provider_with(text_completion("first")))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .open_session(&manager, spec(), DurabilityPolicy::Flush)
        .build()
        .expect("first build succeeds");
    assert_eq!(first.info().session_id, "wf-7.step-2");
    let outcome = first.run("attempt one").await.expect("first run succeeds");
    assert!(outcome.is_completed());

    let retry = AgentBuilder::new(provider_with(text_completion("second")))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .open_session(&manager, spec(), DurabilityPolicy::Flush)
        .build()
        .expect("retry build succeeds");
    let replay = retry.session_replay().expect("resume surfaced replay");
    assert!(
        replay.replayed_events > 0,
        "the retry must replay the first attempt's history",
    );
    assert_eq!(
        manager.list().expect("index readable").len(),
        1,
        "one deterministic id, one session",
    );
}

#[test]
fn open_session_conflicts_with_explicit_session_store() {
    let sessions = tempfile::tempdir().expect("session dir");
    let manager = manager_in(sessions.path());
    let result = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .session(Arc::new(EventStore::new()))
        .open_session(
            &manager,
            SessionSpec::Create { name: None },
            DurabilityPolicy::Flush,
        )
        .build();
    match result {
        Err(NornError::Config(ConfigError::InvalidConfig { reason })) => {
            assert!(reason.contains("open_session"), "{reason}");
        }
        Err(other) => panic!("expected a typed config error, got: {other}"),
        Ok(_) => panic!("session + open_session must fail the build"),
    }
}

#[test]
fn open_session_conflicts_with_explicit_cache_key() {
    let sessions = tempfile::tempdir().expect("session dir");
    let manager = manager_in(sessions.path());
    let result = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .agent_config(AgentLoopConfig {
            cache_key: Some("explicit-key".to_owned()),
            ..AgentLoopConfig::default()
        })
        .open_session(
            &manager,
            SessionSpec::Create { name: None },
            DurabilityPolicy::Flush,
        )
        .build();
    match result {
        Err(NornError::Config(ConfigError::InvalidConfig { reason })) => {
            assert!(reason.contains("cache_key"), "{reason}");
        }
        Err(other) => panic!("expected a typed config error, got: {other}"),
        Ok(_) => panic!("open_session + explicit cache_key must fail the build"),
    }
}

/// A failed open (e.g. resuming a session that does not exist) is a
/// typed build error — never a silent fresh session.
#[test]
fn open_session_resume_of_missing_session_fails_build() {
    let sessions = tempfile::tempdir().expect("session dir");
    let manager = manager_in(sessions.path());
    let result = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .open_session(
            &manager,
            SessionSpec::resume("does-not-exist"),
            DurabilityPolicy::Flush,
        )
        .build();
    match result {
        Err(NornError::Session(_)) => {}
        Err(other) => panic!("expected a session error, got: {other}"),
        Ok(_) => panic!("resuming a missing session must fail the build"),
    }
}

/// `open_session(Fork)` copies the source history into a new session
/// and the agent runs against the fork, leaving the source untouched.
#[tokio::test]
async fn open_session_fork_runs_against_forked_history() {
    let temp = tempfile::tempdir().expect("tempdir");
    let sessions = tempfile::tempdir().expect("session dir");
    let manager = manager_in(sessions.path());

    let source = AgentBuilder::new(provider_with(text_completion("origin")))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .open_session(
            &manager,
            SessionSpec::Create {
                name: Some("source".to_owned()),
            },
            DurabilityPolicy::Flush,
        )
        .build()
        .expect("source build succeeds");
    let source_id = source.session_entry().expect("source entry").id.clone();
    let outcome = source.run("seed history").await.expect("source run");
    assert!(outcome.is_completed());
    let (_, source_read) = manager.read_events(&source_id).expect("source readable");
    let source_len = source_read.events.len();

    let fork = AgentBuilder::new(provider_with(text_completion("forked")))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .open_session(
            &manager,
            SessionSpec::fork(source_id.clone(), Some("fork".to_owned())),
            DurabilityPolicy::Flush,
        )
        .build()
        .expect("fork build succeeds");
    let fork_entry = fork.session_entry().expect("fork entry").clone();
    assert_ne!(fork_entry.id, source_id);
    let replay = fork.session_replay().expect("fork replay");
    assert_eq!(
        replay.replayed_events,
        source_len + 1,
        "fork replays the copied events plus the fork marker",
    );
    let outcome = fork.run("continue on fork").await.expect("fork run");
    assert!(outcome.is_completed());

    let (_, source_after) = manager.read_events(&source_id).expect("source readable");
    assert_eq!(
        source_after.events.len(),
        source_len,
        "the fork's run must not touch the source session",
    );
}
