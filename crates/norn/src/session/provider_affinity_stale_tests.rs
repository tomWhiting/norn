use std::io;

use crate::provider::ProviderStateIdentity;
use crate::session::events::{EventBase, EventUsage, ProviderEpochBoundaryReason, SessionEvent};
use crate::session::manager::ResumePolicy;
use crate::session::persistence::index::{
    ProviderAffinityTransition, validate_or_bind_provider_state_identity,
};
use crate::session::{
    CreateSessionOptions, DurabilityPolicy, EventStore, SessionIndexEntry, SessionManager,
    SessionPersistError,
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

fn append_followup(store: &EventStore, content: &str) -> Result<(), crate::session::SessionError> {
    store.append(SessionEvent::UserMessage {
        base: EventBase::new(store.last_event_id()),
        content: content.to_owned(),
    })?;
    Ok(())
}

fn timeline_bytes(
    data_dir: &std::path::Path,
    entry: &SessionIndexEntry,
) -> Result<Vec<u8>, io::Error> {
    let relative = entry
        .rel_path
        .clone()
        .unwrap_or_else(|| format!("{}.jsonl", entry.id));
    std::fs::read(data_dir.join(relative))
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

#[test]
fn stale_same_identity_validation_reuses_the_winners_single_adoption_cut() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let opened = manager.create(options(), DurabilityPolicy::Flush)?;
    append_response_history(&opened.store)?;
    let stale = opened.entry.clone();
    let session_id = stale.id.clone();
    drop(opened);

    let selected = identity("same-identity-winner");
    let winner = manager
        .open_with_affinity(Some(selected))
        .resume_with_policy(
            &session_id,
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        )?;
    append_followup(&winner.store, "winner advanced beyond adoption")?;
    drop(winner);
    let current = manager.resolve(&session_id)?;
    let before = timeline_bytes(manager.data_dir(), &current)?;

    let binding =
        validate_or_bind_provider_state_identity(manager.data_dir(), &stale, Some(selected), None)?;
    assert!(matches!(
        binding.transition,
        ProviderAffinityTransition::AlreadyBoundByPeer
    ));
    assert_eq!(binding.entry.provider_state_identity, Some(selected));
    assert_eq!(timeline_bytes(manager.data_dir(), &binding.entry)?, before);

    let reopened = manager
        .open_with_affinity(Some(selected))
        .resume_with_policy(
            &session_id,
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        )?;
    assert_eq!(
        reopened
            .store
            .events()
            .iter()
            .filter(|event| is_adoption_boundary(event))
            .count(),
        1,
    );
    assert!(reopened.store.events().iter().any(|event| {
        matches!(
            event,
            SessionEvent::UserMessage { content, .. }
                if content == "winner advanced beyond adoption"
        )
    }));
    Ok(())
}

#[test]
fn stale_loaded_store_must_reopen_after_peer_adopts_the_same_identity() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let stale = manager.create(options(), DurabilityPolicy::Flush)?;
    append_response_history(&stale.store)?;
    let session_id = stale.entry.id.clone();
    let adopter = manager.resume(&session_id, DurabilityPolicy::Flush)?;
    let selected = identity("same-identity-peer");
    adopter
        .store
        .validate_or_bind_provider_state_identity(Some(selected))?;
    append_followup(&adopter.store, "peer advanced after adoption")?;
    let current = manager.resolve(&session_id)?;
    let before = timeline_bytes(manager.data_dir(), &current)?;

    assert!(matches!(
        stale
            .store
            .validate_or_bind_provider_state_identity(Some(selected)),
        Err(SessionPersistError::ProviderStateIdentityReopenRequired)
    ));
    assert_eq!(stale.store.provider_state_identity(), None);
    assert_eq!(timeline_bytes(manager.data_dir(), &current)?, before);
    drop(adopter);
    drop(stale);

    let reopened = manager
        .open_with_affinity(Some(selected))
        .resume_with_policy(
            &session_id,
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        )?;
    assert_eq!(reopened.store.provider_state_identity(), Some(selected));
    assert_eq!(
        reopened
            .store
            .events()
            .iter()
            .filter(|event| is_adoption_boundary(event))
            .count(),
        1,
    );
    assert!(reopened.store.events().iter().any(|event| {
        matches!(
            event,
            SessionEvent::UserMessage { content, .. }
                if content == "peer advanced after adoption"
        )
    }));
    Ok(())
}
