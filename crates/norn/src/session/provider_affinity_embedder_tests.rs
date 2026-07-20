use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::error::SessionError;
use crate::provider::ProviderStateIdentity;
use crate::session::events::{ChildBranchKind, EventBase, EventUsage, SessionEvent};
use crate::session::{
    ChildBranchRequest, ChildDurability, CreateSessionOptions, DurabilityPolicy, EventStore,
    PersistenceSink, SessionBinding, SessionBrancher, SessionManager, SessionPersistError,
    read_session_events_for_entry,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct RejectingAdoptionSink {
    attempts: Arc<AtomicUsize>,
}

struct AmbiguousAdoptionState {
    calls: AtomicUsize,
    durable_event: parking_lot::Mutex<Option<Vec<u8>>>,
}

struct AmbiguousOnceAdoptionSink {
    state: Arc<AmbiguousAdoptionState>,
}

impl PersistenceSink for RejectingAdoptionSink {
    fn persist(&mut self, _event: &SessionEvent) -> Result<(), SessionPersistError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(SessionPersistError::EventStore(
            "adoption boundary rejected".to_owned(),
        ))
    }
}

impl PersistenceSink for AmbiguousOnceAdoptionSink {
    fn persist(&mut self, event: &SessionEvent) -> Result<(), SessionPersistError> {
        let encoded = serde_json::to_vec(event)?;
        self.state.calls.fetch_add(1, Ordering::SeqCst);
        let mut durable_event = self.state.durable_event.lock();
        match durable_event.as_ref() {
            None => {
                *durable_event = Some(encoded);
                Err(SessionPersistError::EventStore(
                    "ambiguous adoption write".to_owned(),
                ))
            }
            Some(existing) if *existing == encoded => Ok(()),
            Some(_) => Err(SessionPersistError::EventStore(
                "adoption retry changed event identity".to_owned(),
            )),
        }
    }
}

fn response_history() -> SessionEvent {
    SessionEvent::AssistantMessage {
        response_items: Vec::new(),
        base: EventBase::new(None),
        content: "prior provider response".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_owned_by_unknown_credentials".to_owned()),
    }
}

fn create_options() -> CreateSessionOptions {
    CreateSessionOptions {
        model: "test-model".to_owned(),
        working_dir: "/work".to_owned(),
        name: None,
    }
}

#[test]
fn failed_sink_adoption_leaves_identity_and_history_unchanged() -> TestResult {
    let attempts = Arc::new(AtomicUsize::new(0));
    let history = response_history();
    let store = EventStore::with_sink_and_events(
        Box::new(RejectingAdoptionSink {
            attempts: Arc::clone(&attempts),
        }),
        vec![history.clone()],
    );
    let before = serde_json::to_vec(&[history])?;
    let identity = ProviderStateIdentity::derive(
        "norn.session.affinity-test",
        b"rejected-adopting-provider-fixture",
    );

    assert!(matches!(
        store.validate_or_bind_provider_state_identity(Some(identity)),
        Err(SessionPersistError::EventStore(reason)) if reason == "adoption boundary rejected"
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(store.provider_state_identity(), None);
    assert_eq!(serde_json::to_vec(&store.events())?, before);
    Ok(())
}

#[test]
fn ambiguous_sink_adoption_retries_the_exact_boundary() -> TestResult {
    let state = Arc::new(AmbiguousAdoptionState {
        calls: AtomicUsize::new(0),
        durable_event: parking_lot::Mutex::new(None),
    });
    let store = EventStore::with_sink_and_events(
        Box::new(AmbiguousOnceAdoptionSink {
            state: Arc::clone(&state),
        }),
        vec![response_history()],
    );
    let identity = ProviderStateIdentity::derive(
        "norn.session.affinity-test",
        b"ambiguously-adopting-provider-fixture",
    );

    assert!(
        store
            .validate_or_bind_provider_state_identity(Some(identity))
            .is_err()
    );
    assert_eq!(store.provider_state_identity(), None);
    assert_eq!(store.len(), 1);

    store.validate_or_bind_provider_state_identity(Some(identity))?;
    let events = store.events();
    let mirrored_boundary = serde_json::to_vec(&events[1])?;
    let durable_event = state.durable_event.lock();
    assert_eq!(state.calls.load(Ordering::SeqCst), 2);
    assert_eq!(store.provider_state_identity(), Some(identity));
    assert_eq!(events.len(), 2);
    assert_eq!(durable_event.as_deref(), Some(mirrored_boundary.as_slice()));
    Ok(())
}

#[test]
fn stale_managed_sink_cannot_append_after_another_handle_adopts() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let stale = manager.create(create_options(), DurabilityPolicy::Flush)?;
    let session_id = stale.entry.id.clone();
    let adopter = manager.resume(&session_id, DurabilityPolicy::Flush)?;
    let identity = ProviderStateIdentity::derive(
        "norn.session.affinity-test",
        b"concurrent-handle-adoption-fixture",
    );
    adopter
        .store
        .validate_or_bind_provider_state_identity(Some(identity))?;
    let current = manager.resolve(&session_id)?;
    let before =
        serde_json::to_vec(&read_session_events_for_entry(manager.data_dir(), &current)?.events)?;

    assert!(matches!(
        stale.store.append(response_history()),
        Err(SessionError::ProviderStateIdentityMismatch)
    ));
    assert_eq!(current.provider_state_identity, Some(identity));
    assert_eq!(
        serde_json::to_vec(&read_session_events_for_entry(manager.data_dir(), &current)?.events)?,
        before,
    );
    Ok(())
}

#[test]
fn persistent_child_inherits_identity_and_stale_parent_cannot_publish() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let identity = ProviderStateIdentity::derive(
        "norn.session.affinity-test",
        b"persistent-child-affinity-fixture",
    );
    let root = manager
        .open_with_affinity(Some(identity))
        .create(create_options(), DurabilityPolicy::Flush)?;
    let brancher = Arc::new(SessionBrancher::new(
        manager.clone(),
        root.entry.id.clone(),
        DurabilityPolicy::Flush,
    ));
    let binding = SessionBinding::persistent_root(brancher, &root.entry, &[]);
    let child_id = uuid::Uuid::new_v4().to_string();
    let child = binding.branch_child(
        &root.store,
        &ChildBranchRequest {
            child_session_id: child_id.clone(),
            name_stem: "worker".to_owned(),
            kind: ChildBranchKind::Spawn,
            durability: ChildDurability::Persist,
            model: "test-model".to_owned(),
            working_dir: "/work".to_owned(),
        },
    )?;
    assert_eq!(
        manager.resolve(&child_id)?.provider_state_identity,
        Some(identity)
    );
    assert_eq!(child.store.provider_state_identity(), Some(identity));

    let stale_root = manager.create(create_options(), DurabilityPolicy::Flush)?;
    let stale_entry = stale_root.entry.clone();
    let adopted = ProviderStateIdentity::derive(
        "norn.session.affinity-test",
        b"stale-parent-adoption-fixture",
    );
    stale_root
        .store
        .validate_or_bind_provider_state_identity(Some(adopted))?;
    let before_count = manager.list()?.len();
    let mut stale_child = stale_entry.clone();
    stale_child.id = uuid::Uuid::new_v4().to_string();
    stale_child.generation = uuid::Uuid::new_v4();
    stale_child.parent_id = Some(stale_entry.id.clone());
    stale_child.rel_path = Some(format!(
        "{}/children/stale-affinity-child.jsonl",
        stale_entry.id
    ));
    assert!(matches!(
        crate::session::persistence::index::publish_new_child_session(
            manager.data_dir(),
            &stale_child,
            &[],
            stale_entry.generation,
            None,
        ),
        Err(SessionPersistError::ProviderStateIdentityMismatch)
    ));
    assert_eq!(manager.list()?.len(), before_count);
    Ok(())
}

#[test]
fn affinity_fork_of_empty_source_rejects_without_binding_or_publication() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let source = manager.create(create_options(), DurabilityPolicy::Flush)?;
    let source_id = source.entry.id.clone();
    let before_index = serde_json::to_vec(&manager.list()?)?;
    let identity =
        ProviderStateIdentity::derive("norn.session.affinity-test", b"empty-fork-affinity-fixture");

    assert!(matches!(
        manager.open_with_affinity(Some(identity)).fork_with_policy(
            &source_id,
            create_options(),
            DurabilityPolicy::Flush,
            crate::session::ResumePolicy::RequireCanonical,
        ),
        Err(SessionPersistError::EmptySource { .. })
    ));
    assert_eq!(serde_json::to_vec(&manager.list()?)?, before_index);
    assert!(
        read_session_events_for_entry(manager.data_dir(), &source.entry)?
            .events
            .is_empty()
    );
    Ok(())
}
