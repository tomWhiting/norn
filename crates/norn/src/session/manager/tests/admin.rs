#[test]
fn rename_sets_and_clears_index_name() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let opened = manager
        .create(options("gpt"), DurabilityPolicy::Flush)
        .unwrap();
    let id = opened.entry.id.clone();
    drop(opened);

    let renamed = manager.rename(&id, Some("milestone".to_owned())).unwrap();
    assert_eq!(renamed.name.as_deref(), Some("milestone"));
    assert_eq!(
        manager.resolve("milestone").unwrap().id,
        id,
        "rename must persist to the index",
    );

    let cleared = manager.rename(&id, None).unwrap();
    assert_eq!(cleared.name, None);
    assert!(
        matches!(
            manager.resolve("milestone").unwrap_err(),
            SessionPersistError::NotFound { .. },
        ),
        "cleared name must no longer resolve",
    );

    let err = manager
        .rename("missing-session", Some("x".to_owned()))
        .unwrap_err();
    assert!(matches!(err, SessionPersistError::NotFound { .. }));
}

#[test]
fn stale_resolved_generation_cannot_rename_recreated_id() -> Result<(), Box<dyn std::error::Error>>
{
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    let first = manager.create_with_id("rename-aba", options("gpt"), DurabilityPolicy::Flush)?;
    let stale = first.entry;
    drop(first.store);
    manager.delete("rename-aba")?;
    let replacement =
        manager.create_with_id("rename-aba", options("gpt"), DurabilityPolicy::Flush)?;
    assert_ne!(stale.generation, replacement.entry.generation);

    let error = manager
        .rename_entry(&stale, Some("stale-name".to_owned()))
        .err()
        .ok_or_else(|| std::io::Error::other("stale rename unexpectedly succeeded"))?;
    assert!(matches!(
        error,
        SessionPersistError::GenerationChanged { .. }
    ));
    assert!(manager.resolve("rename-aba")?.name.is_none());
    Ok(())
}

#[test]
fn delete_removes_file_and_index_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let opened = manager
        .create(options("gpt"), DurabilityPolicy::Flush)
        .unwrap();
    let id = opened.entry.id.clone();
    opened.store.append(user_msg("doomed")).unwrap();
    drop(opened);

    let removed = manager.delete(&id).unwrap();
    assert_eq!(removed.id, id);
    assert!(!session_file_path(tmp.path(), &id).exists());
    assert!(manager.list().unwrap().is_empty());
}

#[test]
fn delete_tolerates_missing_session_file() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    // Index entry with no file (never appended, file removed by hand).
    let entry = new_index_entry("ghost-file".to_owned(), options("gpt"));
    append_index_entry(tmp.path(), &entry, None).unwrap();
    let removed = manager.delete("ghost-file").unwrap();
    assert_eq!(removed.id, "ghost-file");
    assert!(manager.list().unwrap().is_empty());
}

#[test]
fn delete_unknown_session_is_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let err = manager.delete("nonexistent").unwrap_err();
    assert!(matches!(err, SessionPersistError::NotFound { .. }));
}

#[test]
fn read_events_returns_entry_and_strict_replay() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let opened = manager
        .create(options("gpt"), DurabilityPolicy::Flush)
        .unwrap();
    let id = opened.entry.id.clone();
    opened.store.append(user_msg("exported")).unwrap();
    drop(opened);

    let (entry, read) = manager.read_events(&id).unwrap();
    assert_eq!(entry.id, id);
    assert_eq!(read.events.len(), 1);

    let err = manager.read_events("nope-not-here").unwrap_err();
    assert!(matches!(err, SessionPersistError::NotFound { .. }));
}

/// A failed open must never silently degrade to memory-only
/// persistence: occupy the session file path with a directory so
/// the sink open fails.
#[test]
fn open_failure_surfaces_instead_of_degrading() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    fs::create_dir_all(session_file_path(tmp.path(), "occupied")).unwrap();
    let result = manager.open_or_resume("occupied", options("gpt"), DurabilityPolicy::Flush);
    assert!(result.is_err(), "open failure must not be swallowed");
}

/// Root + child fixture for the delete-cascade tests: a persistent
/// root minted through the manager, with one child timeline minted
/// through the branching authority.
fn root_with_child(manager: &SessionManager) -> (String, String, std::path::PathBuf) {
    use crate::session::{ChildBranchRequest, ChildDurability, SessionBinding, SessionBrancher};
    let opened = manager
        .create(options("gpt-root"), DurabilityPolicy::Flush)
        .unwrap();
    let root_id = opened.entry.id.clone();
    let binding = SessionBinding::persistent_root(
        std::sync::Arc::new(SessionBrancher::new(
            manager.clone(),
            root_id.clone(),
            DurabilityPolicy::Flush,
        )),
        &opened.entry,
        &[],
    );
    let child = binding
        .branch_child(
            &opened.store,
            &ChildBranchRequest {
                child_session_id: Uuid::new_v4().to_string(),
                name_stem: "worker".to_owned(),
                kind: crate::session::events::ChildBranchKind::Spawn,
                durability: ChildDurability::Persist,
                model: "gpt-child".to_owned(),
                working_dir: "/w".to_owned(),
            },
        )
        .unwrap();
    let child_id = child.session_id.unwrap();
    let child_entry = manager.resolve(&child_id).unwrap();
    let child_abs = manager
        .data_dir()
        .join(child_entry.rel_path.as_deref().unwrap());
    (root_id, child_id, child_abs)
}

/// F3 cascade: deleting a root removes its file, its `{id}/`
/// directory (child timelines included), the child index rows, and
/// the root row — nothing phantom survives.
#[test]
fn delete_root_cascades_children_dir_and_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let (root_id, child_id, child_abs) = root_with_child(&manager);
    assert!(child_abs.exists(), "fixture: child file on disk");

    manager.delete(&root_id).unwrap();

    assert!(!child_abs.exists(), "child timeline deleted with the root");
    assert!(
        !tmp.path().join(&root_id).exists(),
        "the root's {{id}}/ directory is gone",
    );
    let rows = read_index(tmp.path()).unwrap();
    assert!(
        !rows.iter().any(|e| e.id == root_id || e.id == child_id),
        "no root or child row survives the cascade: {rows:?}",
    );
}

/// F3 crash residue: a previous delete attempt that crashed AFTER
/// `remove_dir_all` but BEFORE the row sweep leaves phantom child
/// rows over deleted files. Re-running delete must still sweep them
/// — the sweep is never gated on the directory existing.
#[test]
fn delete_rerun_sweeps_phantom_child_rows_after_crash_residue() {
    let tmp = tempfile::tempdir().unwrap();
    let manager = SessionManager::new(tmp.path());
    let (root_id, child_id, _child_abs) = root_with_child(&manager);

    // Simulate the crash residue: the children directory is already
    // gone, but the child row (and the root) are still indexed.
    fs::remove_dir_all(tmp.path().join(&root_id)).unwrap();
    assert!(
        read_index(tmp.path())
            .unwrap()
            .iter()
            .any(|e| e.id == child_id),
        "fixture: the phantom child row is present",
    );

    manager.delete(&root_id).unwrap();

    let rows = read_index(tmp.path()).unwrap();
    assert!(
        !rows.iter().any(|e| e.id == root_id || e.id == child_id),
        "the re-run must sweep the phantom child row: {rows:?}",
    );
}
