use std::io::Write as _;

fn directory_names(path: &std::path::Path) -> std::io::Result<Vec<std::ffi::OsString>> {
    let mut names = std::fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    names.sort();
    Ok(names)
}

fn append_orphan_provenance(
    path: &std::path::Path,
    parent_id: Option<crate::session::events::EventId>,
) -> Result<(), Box<dyn std::error::Error>> {
    let boundary = SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(parent_id),
        reason: crate::session::events::ProviderEpochBoundaryReason::ResponseStatePublication,
    };
    let event = crate::session::ProviderStateProvenance::new(
        crate::session::events::EventId::new(),
        true,
    )
    .into_custom_event(EventBase::new(Some(boundary.base().id.clone())))?;
    let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
    serde_json::to_writer(&mut file, &boundary)?;
    file.write_all(b"\n")?;
    serde_json::to_writer(&mut file, &event)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[test]
fn invalid_provenance_precedes_affinity_mutation_and_fork_publication(
) -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let opened = manager.create_with_id(
        "invalid-provider-state",
        options("gpt"),
        DurabilityPolicy::FsyncPerEvent,
    )?;
    let entry = opened.entry.clone();
    let parent_id = opened.store.last_event_id();
    drop(opened);

    let timeline_path = session_file_path(temp.path(), &entry.id);
    append_orphan_provenance(&timeline_path, parent_id)?;
    let index_path = index_file_path(temp.path());
    let timeline_before = std::fs::read(&timeline_path)?;
    let index_before = std::fs::read(&index_path)?;
    let names_before = directory_names(temp.path())?;
    let selected = crate::provider::ProviderStateIdentity::derive(
        "norn.test.provider-state-validation",
        b"selected",
    );

    assert!(matches!(
        manager
            .open_with_affinity(Some(selected))
            .resume_with_policy(
                &entry.id,
                DurabilityPolicy::Flush,
                ResumePolicy::RequireCanonical,
            ),
        Err(SessionPersistError::InvalidProviderStateProvenance(_))
    ));
    assert!(matches!(
        manager.fork(
            &entry.id,
            options("gpt"),
            DurabilityPolicy::Flush,
        ),
        Err(SessionPersistError::InvalidProviderStateProvenance(_))
    ));
    assert!(matches!(
        manager.open_with_affinity(Some(selected)).fork_with_policy(
            &entry.id,
            options("gpt"),
            DurabilityPolicy::Flush,
            ResumePolicy::RequireCanonical,
        ),
        Err(SessionPersistError::InvalidProviderStateProvenance(_))
    ));

    assert_eq!(std::fs::read(&timeline_path)?, timeline_before);
    assert_eq!(std::fs::read(&index_path)?, index_before);
    assert_eq!(directory_names(temp.path())?, names_before);
    let rows = read_index(temp.path())?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].provider_state_identity, None);
    Ok(())
}
