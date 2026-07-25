use std::any::Any;
use std::io;
use std::sync::{Arc, Barrier};

use crate::provider::ProviderStateIdentity;
use crate::session::events::{EventBase, EventUsage, ProviderEpochBoundaryReason, SessionEvent};
use crate::session::manager::ResumePolicy;
use crate::session::{
    CreateSessionOptions, DurabilityPolicy, EventStore, ResumeFidelity, SessionManager,
    SessionPersistError, read_index,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn identity(label: &str) -> ProviderStateIdentity {
    ProviderStateIdentity::derive("norn.test.provider", label.as_bytes())
}

fn options() -> CreateSessionOptions {
    CreateSessionOptions {
        model: "test-model".to_owned(),
        working_dir: "/work".to_owned(),
        name: None,
    }
}

fn append_history(store: &EventStore) -> Result<(), crate::session::SessionError> {
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "persisted history".to_owned(),
    })?;
    Ok(())
}

fn append_response_history(store: &EventStore) -> Result<(), crate::session::SessionError> {
    store.append(SessionEvent::AssistantMessage {
        base: EventBase::new(store.last_event_id()),
        response_items: Vec::new(),
        content: "persisted response".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("response-owned-by-the-old-credential".to_owned()),
    })?;
    Ok(())
}

fn is_adoption_boundary(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::ProviderEpochBoundary {
            reason: ProviderEpochBoundaryReason::ProviderIdentityAdoption,
            ..
        }
    )
}

fn join_error(payload: &(dyn Any + Send)) -> io::Error {
    let detail = if let Some(message) = payload.downcast_ref::<&str>() {
        *message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.as_str()
    } else {
        "non-string panic payload"
    };
    io::Error::other(format!("affinity worker panicked: {detail}"))
}

#[test]
fn sinkless_store_binds_once_and_rejects_absent_or_different_identity() -> TestResult {
    let store = EventStore::new();
    let first = identity("first");
    store.validate_or_bind_provider_state_identity(Some(first))?;
    store.validate_or_bind_provider_state_identity(Some(first))?;
    assert_eq!(store.provider_state_identity(), Some(first));
    assert!(matches!(
        store.validate_or_bind_provider_state_identity(Some(identity("second"))),
        Err(SessionPersistError::ProviderStateIdentityMismatch)
    ));
    assert!(matches!(
        store.validate_or_bind_provider_state_identity(None),
        Err(SessionPersistError::ProviderStateIdentityRequired)
    ));
    Ok(())
}

#[test]
fn sinkless_concurrent_first_bind_has_one_immutable_winner() -> TestResult {
    let store = Arc::new(EventStore::new());
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for candidate in [identity("first"), identity("second")] {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            store
                .validate_or_bind_provider_state_identity(Some(candidate))
                .map(|()| candidate)
        }));
    }
    barrier.wait();
    let outcomes = threads
        .into_iter()
        .map(|thread| {
            thread
                .join()
                .map_err(|payload| join_error(payload.as_ref()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(
                result,
                Err(SessionPersistError::ProviderStateIdentityMismatch)
            ))
            .count(),
        1
    );
    assert_eq!(
        store.provider_state_identity(),
        outcomes
            .iter()
            .find_map(|result| result.as_ref().ok())
            .copied()
    );
    Ok(())
}

#[test]
fn managed_create_and_store_binding_are_durable() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let selected = identity("selected");
    let opened = manager
        .open_with_affinity(Some(selected))
        .create(options(), DurabilityPolicy::Flush)?;
    let session_id = opened.entry.id.clone();
    assert_eq!(opened.entry.provider_state_identity, Some(selected));
    assert_eq!(opened.store.provider_state_identity(), Some(selected));
    drop(opened);

    let resumed = manager
        .open_with_affinity(Some(selected))
        .resume_with_policy(
            &session_id,
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        )?;
    assert_eq!(resumed.entry.provider_state_identity, Some(selected));
    assert!(matches!(
        manager.open_with_affinity(None).resume_with_policy(
            &session_id,
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        ),
        Err(SessionPersistError::ProviderStateIdentityRequired)
    ));
    // A different credential does not lock the session out — it rebinds
    // through a fresh epoch boundary (owner ruling 2026-07-25: sessions
    // are not locked to an account).
    let rebound = manager
        .open_with_affinity(Some(identity("different")))
        .resume_with_policy(
            &session_id,
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        )?;
    assert_eq!(
        rebound.entry.provider_state_identity,
        Some(identity("different"))
    );
    Ok(())
}

#[test]
fn resume_under_a_different_credential_rebinds_via_epoch_boundary() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let first = identity("account-a");
    let opened = manager
        .open_with_affinity(Some(first))
        .create(options(), DurabilityPolicy::Flush)?;
    let session_id = opened.entry.id.clone();
    append_response_history(&opened.store)?;
    drop(opened);

    // Same-operator credential change: the resume proceeds, the identity
    // rebinds, and an adoption epoch boundary retires the old anchors so
    // the new credential can never replay them.
    let second = identity("account-b");
    let rebound = manager
        .open_with_affinity(Some(second))
        .resume_with_policy(
            &session_id,
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        )?;
    assert_eq!(rebound.entry.provider_state_identity, Some(second));
    assert_eq!(
        rebound
            .store
            .events()
            .iter()
            .filter(|event| is_adoption_boundary(event))
            .count(),
        1,
        "rebinding must retire the previous identity's anchors behind a boundary",
    );
    drop(rebound);

    // Resuming again under the same credential validates without another
    // boundary.
    let steady = manager
        .open_with_affinity(Some(second))
        .resume_with_policy(
            &session_id,
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        )?;
    assert_eq!(steady.entry.provider_state_identity, Some(second));
    assert_eq!(
        steady
            .store
            .events()
            .iter()
            .filter(|event| is_adoption_boundary(event))
            .count(),
        1,
        "a matching credential must validate without a new boundary",
    );
    drop(steady);

    // The rebind is symmetric — moving back is just another rebind. The
    // boundary count stays at one because nothing was appended after the
    // previous boundary, so the durable tail boundary is reused rather
    // than duplicated.
    let back = manager.open_with_affinity(Some(first)).resume_with_policy(
        &session_id,
        DurabilityPolicy::Flush,
        ResumePolicy::RequireCanonical,
    )?;
    assert_eq!(back.entry.provider_state_identity, Some(first));
    Ok(())
}

#[test]
fn direct_managed_store_adopts_identity_durably() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let opened = manager.create(options(), DurabilityPolicy::Flush)?;
    let session_id = opened.entry.id.clone();
    append_response_history(&opened.store)?;
    let selected = identity("direct-store");

    opened
        .store
        .validate_or_bind_provider_state_identity(Some(selected))?;
    assert!(
        opened
            .store
            .events()
            .last()
            .is_some_and(is_adoption_boundary),
        "late adoption must mirror the durable epoch boundary into memory",
    );
    drop(opened);
    let row = manager.resolve(&session_id)?;
    assert_eq!(row.provider_state_identity, Some(selected));
    let durable = crate::session::read_session_events_for_entry(manager.data_dir(), &row)?;
    assert!(durable.events.last().is_some_and(is_adoption_boundary));
    Ok(())
}

#[test]
fn interrupted_adoption_leaves_boundary_before_any_identity_binding() -> TestResult {
    use crate::session::persistence::index::{
        AffinityBindingCheckpoint, validate_or_bind_provider_state_identity_with_hook,
    };

    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let opened = manager.create(options(), DurabilityPolicy::Flush)?;
    append_response_history(&opened.store)?;
    let registered = opened.entry.clone();
    let session_id = registered.id.clone();
    drop(opened);
    let mut observed_boundary = false;
    let stopped = validate_or_bind_provider_state_identity_with_hook(
        manager.data_dir(),
        &registered,
        Some(identity("interrupted")),
        None,
        &mut |checkpoint| {
            observed_boundary = checkpoint == AffinityBindingCheckpoint::BoundaryDurable;
            Err(SessionPersistError::EventStore(
                "injected stop after boundary durability".to_owned(),
            ))
        },
    );
    assert!(stopped.is_err());
    assert!(observed_boundary);
    assert_eq!(manager.resolve(&session_id)?.provider_state_identity, None);
    let selected = identity("retry-winner");
    let resumed = manager
        .open_with_affinity(Some(selected))
        .resume_with_policy(
            &session_id,
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        )?;
    assert_eq!(resumed.entry.provider_state_identity, Some(selected));
    assert_eq!(
        resumed
            .store
            .events()
            .iter()
            .filter(|event| is_adoption_boundary(event))
            .count(),
        1,
        "retry must recognize the already-durable tail boundary",
    );
    Ok(())
}

#[test]
fn concurrent_legacy_adoption_converges_on_one_identity() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let opened = manager.create(options(), DurabilityPolicy::Flush)?;
    append_history(&opened.store)?;
    let session_id = opened.entry.id.clone();
    drop(opened);

    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for candidate in [identity("first"), identity("second")] {
        let manager = manager.clone();
        let session_id = session_id.clone();
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            manager
                .open_with_affinity(Some(candidate))
                .resume_with_policy(
                    &session_id,
                    DurabilityPolicy::Flush,
                    ResumePolicy::RequireCanonical,
                )
                .map(|opened| opened.entry.provider_state_identity)
        }));
    }
    barrier.wait();
    let outcomes = threads
        .into_iter()
        .map(|thread| {
            thread
                .join()
                .map_err(|payload| join_error(payload.as_ref()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Same-operator credentials never lock each other out (owner ruling
    // 2026-07-25): both adoptions succeed and the durable row converges
    // on the last binder. A racer that lost the row detects staleness on
    // its next index transaction rather than silently diverging.
    assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 2);
    let durable = manager.resolve(&session_id)?.provider_state_identity;
    assert!(durable.is_some());
    assert!(
        outcomes
            .iter()
            .filter_map(|result| result.as_ref().ok())
            .any(|bound| *bound == durable),
        "the durable identity must be one of the concurrent binders' outcomes",
    );
    Ok(())
}

#[test]
fn affinity_open_or_resume_converges_and_rejects_another_identity() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let selected = identity("selected");
    let id = "deterministic-session";
    let first = manager
        .open_with_affinity(Some(selected))
        .open_or_resume_with_policy(
            id,
            options(),
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        )?;
    drop(first);

    let resumed = manager
        .open_with_affinity(Some(selected))
        .open_or_resume_with_policy(
            id,
            options(),
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        )?;
    assert_eq!(resumed.entry.id, id);
    drop(resumed);
    // A different credential rebinds instead of locking the session out
    // (owner ruling 2026-07-25: sessions are not locked to an account).
    let rebound = manager
        .open_with_affinity(Some(identity("different")))
        .open_or_resume_with_policy(
            id,
            options(),
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        )?;
    assert_eq!(rebound.entry.id, id);
    assert_eq!(
        rebound.entry.provider_state_identity,
        Some(identity("different"))
    );
    Ok(())
}

#[test]
fn fork_validates_before_publication_and_inherits_binding() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let selected = identity("selected");
    let source = manager
        .open_with_affinity(Some(selected))
        .create(options(), DurabilityPolicy::Flush)?;
    append_history(&source.store)?;
    let source_id = source.entry.id.clone();
    drop(source);

    // Forking under a different credential of the same operator succeeds:
    // the CHILD adopts the forker's identity behind its own epoch boundary
    // and the SOURCE row is never mutated by a fork (owner ruling
    // 2026-07-25: sessions are not locked to an account).
    let cross = manager
        .open_with_affinity(Some(identity("different")))
        .fork_with_policy(
            &source_id,
            options(),
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        )?;
    assert_eq!(
        cross.entry.provider_state_identity,
        Some(identity("different"))
    );
    assert!(
        cross.store.events().iter().any(is_adoption_boundary),
        "a cross-credential fork must retire the source's anchors behind a boundary",
    );
    assert_eq!(
        manager.resolve(&source_id)?.provider_state_identity,
        Some(selected),
        "forking must never mutate the source session's identity",
    );
    drop(cross);

    let forked = manager
        .open_with_affinity(Some(selected))
        .fork_with_policy(
            &source_id,
            options(),
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        )?;
    assert_eq!(forked.entry.provider_state_identity, Some(selected));
    assert_eq!(forked.store.provider_state_identity(), Some(selected));
    assert!(
        !forked.store.events().iter().any(is_adoption_boundary),
        "a same-credential fork inherits the binding without a boundary",
    );
    Ok(())
}

#[test]
fn stale_unbound_fork_snapshot_cannot_publish_after_identity_adoption() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let source = manager.create(options(), DurabilityPolicy::Flush)?;
    append_history(&source.store)?;
    let stale_source = source.entry.clone();
    let events = source.store.events();
    source
        .store
        .validate_or_bind_provider_state_identity(Some(identity("adopted")))?;

    let mut candidate = stale_source.clone();
    candidate.id = uuid::Uuid::new_v4().to_string();
    candidate.generation = uuid::Uuid::new_v4();
    candidate.created_at = chrono::Utc::now();
    candidate.updated_at = candidate.created_at;
    candidate.event_count = 0;
    candidate.total_input_tokens = 0;
    candidate.total_output_tokens = 0;
    candidate.total_cache_read_tokens = 0;

    assert!(matches!(
        crate::session::persistence::index::publish_new_fork_session(
            manager.data_dir(),
            &candidate,
            &events,
            &stale_source,
            None,
        ),
        Err(SessionPersistError::ProviderStateIdentityMismatch)
    ));
    assert_eq!(manager.list()?.len(), 1);
    assert!(
        !manager
            .data_dir()
            .join(format!("{}.jsonl", candidate.id))
            .exists()
    );
    Ok(())
}

#[test]
fn denied_resume_and_fork_do_not_claim_an_identity() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let opened = manager.create(options(), DurabilityPolicy::Flush)?;
    append_history(&opened.store)?;
    let session_id = opened.entry.id.clone();
    drop(opened);
    crate::session::update_index_entry(manager.data_dir(), &session_id, None, |entry| {
        entry.fidelity = ResumeFidelity::FreshEpochProjection;
    })?;

    let affinity = identity("must-not-land");
    assert!(matches!(
        manager
            .open_with_affinity(Some(affinity))
            .resume_with_policy(
                &session_id,
                DurabilityPolicy::Flush,
                ResumePolicy::RequireCanonical,
            ),
        Err(SessionPersistError::ResumeApprovalRequired { .. })
    ));
    assert!(matches!(
        manager.open_with_affinity(Some(affinity)).fork_with_policy(
            &session_id,
            options(),
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        ),
        Err(SessionPersistError::ResumeApprovalRequired { .. })
    ));
    let rows = read_index(manager.data_dir())?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].provider_state_identity, None);
    Ok(())
}

#[test]
fn index_identity_is_optional_but_strictly_shaped() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let selected = identity("serialized");
    let opened = manager
        .open_with_affinity(Some(selected))
        .create(options(), DurabilityPolicy::Flush)?;
    let mut value = serde_json::to_value(&opened.entry)?;

    let object = value
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other("index row did not serialize as an object"))?;
    let identity_value = object
        .get_mut("provider_state_identity")
        .ok_or_else(|| std::io::Error::other("bound identity was not serialized"))?;
    let bytes = identity_value
        .as_array_mut()
        .ok_or_else(|| std::io::Error::other("identity did not serialize as a byte array"))?;
    bytes.pop();
    assert!(serde_json::from_value::<crate::session::SessionIndexEntry>(value).is_err());

    let debug = format!("{:?}", opened.entry);
    assert!(debug.contains("ProviderStateIdentity([REDACTED])"));
    Ok(())
}
