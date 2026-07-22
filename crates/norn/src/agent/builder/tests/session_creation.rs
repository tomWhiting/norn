use super::*;

// -- open_session: the managed persisted-session path -------------------

fn manager_in(dir: &std::path::Path) -> SessionManager {
    SessionManager::new(dir)
}

/// `open_session(Create)` wires the persisted session end to end:
/// the index entry records the resolved model and working dir, the
/// entry id becomes the cache key, the environment session id, and
/// the introspected session id, and a run's events persist to disk.
#[tokio::test]
async fn open_session_create_wires_store_cache_key_and_session_id()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let sessions = tempfile::tempdir()?;
    let manager = manager_in(sessions.path());

    let agent = AgentBuilder::new(provider_with(text_completion("persisted")))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .workspace_root(temp.path())
        .open_session(
            &manager,
            SessionSpec::Create {
                name: Some("track-h".to_owned()),
            },
            DurabilityPolicy::Flush,
        )
        .build()?;

    let entry = agent
        .session_entry()
        .ok_or_else(|| std::io::Error::other("opened session entry was not surfaced"))?
        .clone();
    assert_eq!(entry.model, "test-model", "entry records resolved model");
    assert_eq!(
        entry.working_dir,
        temp.path().canonicalize()?.display().to_string(),
        "entry records resolved working dir",
    );
    assert_eq!(entry.name.as_deref(), Some("track-h"));
    assert_eq!(agent.config.cache_key.as_deref(), Some(entry.id.as_str()));
    assert_eq!(agent.info().session_id, entry.id);
    let ctx = agent
        .registry
        .shared_context()
        .ok_or_else(|| std::io::Error::other("shared tool context missing"))?;
    let artifacts = ctx
        .get_extension::<crate::session::SessionArtifactStore>()
        .ok_or_else(|| std::io::Error::other("session artifact authority missing"))?;
    let expected_artifact_root = sessions
        .path()
        .join(&entry.id)
        .join("artifacts")
        .canonicalize()?;
    let session_data_root = sessions.path().canonicalize()?;
    assert_eq!(
        artifacts.readable_root().canonicalize()?,
        expected_artifact_root
    );
    assert!(ctx.read_exempt_roots().contains(&expected_artifact_root));
    assert!(
        !ctx.read_exempt_roots()
            .iter()
            .any(|root| root == &session_data_root),
        "the session data root itself must never become model-readable",
    );

    let artifact = artifacts.write_fetched("https://example.test", "artifact body")?;
    let transcript = sessions.path().join(format!("{}.jsonl", entry.id));
    let read = agent
        .registry
        .get("read")
        .ok_or_else(|| std::io::Error::other("read tool missing from the default registry"))?;
    let read_envelope = |path: &std::path::Path| crate::tool::envelope::ToolEnvelope {
        tool_call_id: "tc-session-artifact".to_owned(),
        tool_name: "read".to_owned(),
        model_args: serde_json::json!({ "path": path.display().to_string() }),
        metadata: Value::Null,
    };
    let allowed = read
        .execute(&read_envelope(&artifact), ctx.as_ref())
        .await?;
    assert!(!allowed.is_error(), "the session artifact must be readable");
    let denied = read
        .execute(&read_envelope(&transcript), ctx.as_ref())
        .await?;
    assert!(denied.is_error(), "the sibling transcript must stay denied");
    assert_eq!(denied.content["error"]["kind"], "permission_denied");
    assert_eq!(
        agent
            .loop_context
            .environment
            .as_ref()
            .and_then(|env| env.session_id.as_deref()),
        Some(entry.id.as_str()),
        "the system prompt environment carries the persisted session id",
    );
    assert_eq!(
        agent.session_replay(),
        Some(crate::session::ReplaySummary::default()),
        "a fresh create replays nothing",
    );

    let outcome = agent.run("persist me").await?;
    assert!(outcome.is_completed());

    // The run's events landed in the managed session on disk.
    let (_, read) = manager.read_events(&entry.id)?;
    assert!(
        !read.events.is_empty(),
        "run events must persist through the managed sink",
    );
    Ok(())
}

/// Child-persistence V2: `open_session` arms the root's persistent
/// [`crate::session::SessionBinding`] on the coordination infra —
/// the single allocation authority every spawn/fork child mint
/// routes through — keyed to the opened session's id. Without
/// `open_session` (in-memory `.session(..)` or nothing) the binding
/// is the deliberate ephemeral root.
#[tokio::test]
async fn open_session_installs_persistent_branching_binding() {
    let temp = tempfile::tempdir().expect("tempdir");
    let sessions = tempfile::tempdir().expect("session dir");
    let manager = manager_in(sessions.path());

    let agent = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .agent_registry(AgentRegistry::shared())
        .child_policy(test_child_policy())
        .child_result_capacity(16)
        .open_session(
            &manager,
            SessionSpec::Create { name: None },
            DurabilityPolicy::Flush,
        )
        .build()
        .expect("build succeeds");
    let entry_id = agent.session_entry().expect("entry").id.clone();
    let infra = agent
        .registry
        .shared_context()
        .expect("shared tool context")
        .get_extension::<crate::tools::agent::AgentToolInfra>()
        .expect("coordination infra installed");
    assert!(
        infra.session.is_persistent(),
        "an opened session must arm a persistent branching binding",
    );
    assert_eq!(
        infra.session.session_id(),
        Some(entry_id.as_str()),
        "the binding is keyed to the opened session",
    );
    assert_eq!(infra.session.path_address(), "root");

    // No open_session: the binding is the deliberate ephemeral root.
    let ephemeral = AgentBuilder::new(provider_with(vec![]))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        .agent_registry(AgentRegistry::shared())
        .child_policy(test_child_policy())
        .child_result_capacity(16)
        .build()
        .expect("build succeeds");
    let infra = ephemeral
        .registry
        .shared_context()
        .expect("shared tool context")
        .get_extension::<crate::tools::agent::AgentToolInfra>()
        .expect("coordination infra installed");
    assert!(
        !infra.session.is_persistent(),
        "without a persisted session the binding is honestly ephemeral",
    );
    assert!(
        ephemeral
            .registry
            .shared_context()
            .and_then(|ctx| ctx.get_extension::<crate::session::SessionArtifactStore>())
            .is_none(),
        "an ephemeral run must not invent a durable artifact owner",
    );
}

/// F9 end-to-end: a builder WITHOUT a persisted session (the
/// `--no-session` shape: an in-memory `.session(..)` store) arms the
/// ephemeral binding, and a real spawn through the built agent's
/// tool surface records the honest `session: None` `ChildBranch`
/// reservation on the root's store — absence stated typed, never a
/// fake id, and no session file or index anywhere.
#[tokio::test]
async fn no_session_spawn_records_honest_none_branch_event() {
    use crate::session::events::SessionEvent;
    use crate::tool::envelope::ToolEnvelope;
    use crate::tool::traits::Tool as _;

    let agent = AgentBuilder::new(provider_with(text_completion("child done")))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(std::env::temp_dir())
        .agent_registry(AgentRegistry::shared())
        .child_policy(test_child_policy())
        .child_result_capacity(16)
        .session(Arc::new(crate::session::EventStore::new()))
        .build()
        .expect("build succeeds");
    let shared = agent
        .registry
        .shared_context()
        .expect("shared tool context");

    let tool = crate::tools::agent::SpawnAgentTool::new();
    let out = tool
        .execute(
            &ToolEnvelope {
                tool_call_id: "call-ephemeral".to_owned(),
                tool_name: "spawn_agent".to_owned(),
                model_args: serde_json::json!({
                    "task": "t",
                    "model": crate::model_catalog::default_selection().model,
                    "role": "worker",
                }),
                metadata: serde_json::Value::Null,
            },
            shared.as_ref(),
        )
        .await
        .expect("spawn under an ephemeral root succeeds");
    assert!(!out.is_error(), "{:?}", out.content);

    let reservation = agent
        .event_store
        .events()
        .iter()
        .find_map(|e| match e {
            SessionEvent::ChildBranch {
                parent_session_id,
                child_session_id,
                path_address,
                ..
            } => Some((
                parent_session_id.clone(),
                child_session_id.clone(),
                path_address.clone(),
            )),
            _ => None,
        })
        .expect("the no-session root's store carries the ChildBranch reservation");
    assert_eq!(
        reservation.0, None,
        "--no-session root: parent_session_id is honest None",
    );
    assert_eq!(
        reservation.1, None,
        "--no-session child: child_session_id is honest None, never a fake id",
    );
    assert!(
        reservation.2.starts_with("root/worker-"),
        "the name is still reserved on the parent timeline: {}",
        reservation.2,
    );
}

/// Gap 14 closure: the `RunOutcome` payload carries the LIVE
/// sink-equipped store `Arc` — appends made by the embedder AFTER
/// the run still write through to disk, even with the fork/spawn
/// coordination infra installed (the `Arc` cycle that used to force
/// a silent sink-less snapshot).
#[tokio::test]
async fn run_outcome_store_keeps_persisting_after_run() {
    use crate::session::events::{EventBase, SessionEvent};

    let temp = tempfile::tempdir().expect("tempdir");
    let sessions = tempfile::tempdir().expect("session dir");
    let manager = manager_in(sessions.path());

    let outcome = AgentBuilder::new(provider_with(text_completion("done")))
        .model("test-model")
        .context_window_limit(TEST_CONTEXT_WINDOW)
        .working_dir(temp.path())
        // Coordination infra installed: this is exactly the shape
        // whose Arc cycle used to trigger the snapshot fallback.
        .agent_registry(AgentRegistry::shared())
        .child_policy(test_child_policy())
        .child_result_capacity(16)
        .open_session(
            &manager,
            SessionSpec::Create { name: None },
            DurabilityPolicy::Flush,
        )
        .run("first turn")
        .await
        .expect("run succeeds");
    let store = outcome
        .into_output()
        .event_store
        .expect("event store returned");

    // Post-run embedder append: must reach the SAME on-disk session.
    store
        .append(SessionEvent::UserMessage {
            base: EventBase::new(store.last_event_id()),
            content: "appended after the run".to_owned(),
        })
        .expect("post-run append accepted");

    let index = crate::session::read_index(sessions.path()).expect("index");
    let entry = &index[0];
    let read =
        crate::session::read_session_events(sessions.path(), &entry.id).expect("session readable");
    assert!(
        read.events.iter().any(|e| matches!(
            e,
            SessionEvent::UserMessage { content, .. }
                if content == "appended after the run"
        )),
        "a post-run append must still write through to disk — the \
         returned store carries the live sink, never a snapshot",
    );
}
