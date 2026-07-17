use crate::session::events::ProviderEpochBoundaryReason;

#[test]
fn canonical_migrated_resume_appends_one_boundary_across_restart()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let entry = seed_migrated(&manager, ResumeFidelity::Canonical, false)?;

    let first = manager.resume(&entry.id, DurabilityPolicy::Flush)?;
    assert_eq!(migrated_boundary_count(&first.store.events()), 1);
    assert_eq!(first.entry.event_count, 1);
    drop(first);

    let second = manager.resume(&entry.id, DurabilityPolicy::Flush)?;
    assert_eq!(migrated_boundary_count(&second.store.events()), 1);
    assert_eq!(second.replay.replayed_events, 1);
    Ok(())
}

#[test]
fn concurrent_migrated_resumes_converge_on_one_boundary() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let entry = seed_migrated(&manager, ResumeFidelity::Canonical, false)?;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let manager = manager.clone();
        let id = entry.id.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            barrier.wait();
            manager
                .resume(&id, DurabilityPolicy::Flush)
                .map(|opened| migrated_boundary_count(&opened.store.events()))
        }));
    }
    for worker in workers {
        let count = worker
            .join()
            .map_err(|_panic| std::io::Error::other("resume worker panicked"))??;
        assert_eq!(count, 1);
    }
    let (_, replay) = manager.read_events(&entry.id)?;
    assert_eq!(migrated_boundary_count(&replay.events), 1);
    Ok(())
}

#[test]
fn fresh_projection_refuses_implicit_resume_and_accepts_explicit_policy()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let entry = seed_migrated(&manager, ResumeFidelity::FreshEpochProjection, false)?;

    let refusal = manager
        .resume(&entry.id, DurabilityPolicy::Flush)
        .err()
        .ok_or_else(|| std::io::Error::other("degraded resume unexpectedly succeeded"))?;
    assert!(matches!(
        refusal,
        SessionPersistError::ResumeApprovalRequired { id } if id == entry.id
    ));

    let approved = manager.resume_with_policy(
        &entry.id,
        DurabilityPolicy::Flush,
        ResumePolicy::ApproveFreshEpochProjection,
    )?;
    assert_eq!(migrated_boundary_count(&approved.store.events()), 1);
    Ok(())
}

#[test]
fn inspect_only_policy_is_typed_refusal() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let mut entry = native_entry("inspect-policy", ResumeFidelity::Canonical);
    entry.fidelity = ResumeFidelity::InspectOnly;

    let error =
        super::resume_policy::authorize_resume(&entry, ResumePolicy::ApproveFreshEpochProjection)
            .err()
            .ok_or_else(|| std::io::Error::other("inspect-only policy unexpectedly succeeded"))?;
    assert!(matches!(
        error,
        SessionPersistError::SessionNotResumable { id } if id == entry.id
    ));
    assert!(manager.list()?.is_empty());
    Ok(())
}

#[test]
fn later_native_response_follows_boundary_and_can_reanchor()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let entry = seed_migrated(&manager, ResumeFidelity::Canonical, false)?;
    let resumed = manager.resume(&entry.id, DurabilityPolicy::Flush)?;

    resumed.store.append(SessionEvent::AssistantMessage {
        base: EventBase::new(resumed.store.last_event_id()),
        response_items: Vec::new(),
        content: "new epoch".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "stop".to_owned(),
        response_id: Some("resp_new_epoch".to_owned()),
    })?;
    resumed.store.checkpoint()?;

    let events = resumed.store.events();
    assert_eq!(migrated_boundary_count(&events), 1);
    assert!(matches!(
        events.last(),
        Some(SessionEvent::AssistantMessage {
            response_id: Some(response_id),
            ..
        }) if response_id == "resp_new_epoch"
    ));
    Ok(())
}

#[test]
fn approved_fork_preserves_degraded_lineage_and_one_boundary()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let source = seed_migrated(&manager, ResumeFidelity::FreshEpochProjection, true)?;

    let refusal = manager
        .fork(&source.id, options("fork-model"), DurabilityPolicy::Flush)
        .err()
        .ok_or_else(|| std::io::Error::other("degraded fork unexpectedly succeeded"))?;
    assert!(matches!(
        refusal,
        SessionPersistError::ResumeApprovalRequired { id } if id == source.id
    ));

    let fork = manager.fork_with_policy(
        &source.id,
        options("fork-model"),
        DurabilityPolicy::Flush,
        ResumePolicy::ApproveFreshEpochProjection,
    )?;
    assert_eq!(fork.entry.fidelity, ResumeFidelity::FreshEpochProjection);
    assert_eq!(fork.entry.origin, source.origin);
    assert_eq!(migrated_boundary_count(&fork.store.events()), 1);
    Ok(())
}

#[test]
fn open_or_resume_existing_arm_obeys_explicit_policy() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let entry = seed_migrated(&manager, ResumeFidelity::FreshEpochProjection, false)?;

    let refusal = manager
        .open_or_resume(&entry.id, options("ignored"), DurabilityPolicy::Flush)
        .err()
        .ok_or_else(|| std::io::Error::other("implicit open-or-resume unexpectedly succeeded"))?;
    assert!(matches!(
        refusal,
        SessionPersistError::ResumeApprovalRequired { .. }
    ));
    let approved = manager.open_or_resume_with_policy(
        &entry.id,
        options("ignored"),
        DurabilityPolicy::Flush,
        ResumePolicy::ApproveFreshEpochProjection,
    )?;
    assert_eq!(migrated_boundary_count(&approved.store.events()), 1);
    Ok(())
}

fn seed_migrated(
    manager: &SessionManager,
    fidelity: ResumeFidelity,
    with_event: bool,
) -> Result<SessionIndexEntry, Box<dyn std::error::Error>> {
    let id = uuid::Uuid::new_v4().to_string();
    let mut entry = native_entry(&id, fidelity);
    entry.origin = SessionRecordOrigin::MigratedLegacy {
        source_format: 1,
        source_sha256: "a".repeat(64),
    };
    let events = with_event.then(|| user_msg("legacy history"));
    let entry = publish_new_session(
        manager.data_dir(),
        &entry,
        events.as_slice(),
        None,
    )?;
    Ok(entry)
}

fn native_entry(id: &str, fidelity: ResumeFidelity) -> SessionIndexEntry {
    let now = chrono::Utc::now();
    SessionIndexEntry {
        id: id.to_owned(),
        generation: uuid::Uuid::new_v4(),
        name: None,
        model: "test-model".to_owned(),
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
        fidelity,
        origin: SessionRecordOrigin::Native,
    }
}

fn migrated_boundary_count(events: &[SessionEvent]) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                SessionEvent::ProviderEpochBoundary {
                    reason: ProviderEpochBoundaryReason::MigratedLegacy,
                    ..
                }
            )
        })
        .count()
}
