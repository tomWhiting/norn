type AdminTestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[test]
fn rename_sets_and_clears_index_name() -> AdminTestResult {
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    let opened = manager.create(options("gpt"), DurabilityPolicy::Flush)?;
    let id = opened.entry.id.clone();
    drop(opened);

    let renamed = manager.rename(&id, Some("milestone".to_owned()))?;
    assert_eq!(renamed.name.as_deref(), Some("milestone"));
    assert_eq!(
        manager.resolve("milestone")?.id,
        id,
        "rename must persist to the index",
    );

    let cleared = manager.rename(&id, None)?;
    assert_eq!(cleared.name, None);
    let cleared_error = manager
        .resolve("milestone")
        .err()
        .ok_or_else(|| std::io::Error::other("cleared name unexpectedly resolved"))?;
    assert!(
        matches!(cleared_error, SessionPersistError::NotFound { .. }),
        "cleared name must no longer resolve",
    );

    let err = manager
        .rename("missing-session", Some("x".to_owned()))
        .err()
        .ok_or_else(|| std::io::Error::other("missing session unexpectedly renamed"))?;
    assert!(matches!(err, SessionPersistError::NotFound { .. }));
    Ok(())
}

#[test]
fn stale_resolved_generation_cannot_rename_recreated_id() -> AdminTestResult {
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
fn delete_removes_file_and_index_entry() -> AdminTestResult {
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    let opened = manager.create(options("gpt"), DurabilityPolicy::Flush)?;
    let id = opened.entry.id.clone();
    opened.store.append(user_msg("doomed"))?;
    drop(opened);

    let removed = manager.delete(&id)?;
    assert_eq!(removed.id, id);
    assert!(!session_file_path(tmp.path(), &id).exists());
    assert!(manager.list()?.is_empty());
    Ok(())
}

#[test]
fn delete_tolerates_missing_session_file() -> AdminTestResult {
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    // Index entry with no file (never appended, file removed by hand).
    let entry = new_index_entry("ghost-file".to_owned(), options("gpt"));
    append_index_entry(tmp.path(), &entry, None)?;
    let removed = manager.delete("ghost-file")?;
    assert_eq!(removed.id, "ghost-file");
    assert!(manager.list()?.is_empty());
    Ok(())
}

#[test]
fn delete_unknown_session_is_not_found() -> AdminTestResult {
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    let err = manager
        .delete("nonexistent")
        .err()
        .ok_or_else(|| std::io::Error::other("unknown session unexpectedly deleted"))?;
    assert!(matches!(err, SessionPersistError::NotFound { .. }));
    Ok(())
}

#[test]
fn read_events_returns_entry_and_strict_replay() -> AdminTestResult {
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    let opened = manager.create(options("gpt"), DurabilityPolicy::Flush)?;
    let id = opened.entry.id.clone();
    opened.store.append(user_msg("exported"))?;
    drop(opened);

    let (entry, read) = manager.read_events(&id)?;
    assert_eq!(entry.id, id);
    assert_eq!(read.events.len(), 1);

    let err = manager
        .read_events("nope-not-here")
        .err()
        .ok_or_else(|| std::io::Error::other("unknown session unexpectedly read"))?;
    assert!(matches!(err, SessionPersistError::NotFound { .. }));
    Ok(())
}

/// A failed open must never silently degrade to memory-only
/// persistence: occupy the session file path with a directory so
/// the sink open fails.
#[test]
fn open_failure_surfaces_instead_of_degrading() -> AdminTestResult {
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    fs::create_dir_all(session_file_path(tmp.path(), "occupied"))?;
    let result = manager.open_or_resume("occupied", options("gpt"), DurabilityPolicy::Flush);
    assert!(result.is_err(), "open failure must not be swallowed");
    Ok(())
}

/// Root + child fixture for the delete-cascade tests: a persistent
/// root minted through the manager, with one child timeline minted
/// through the branching authority.
fn root_with_child(
    manager: &SessionManager,
) -> AdminTestResult<(String, String, std::path::PathBuf)> {
    use crate::session::{ChildBranchRequest, ChildDurability, SessionBinding, SessionBrancher};
    let opened = manager.create(options("gpt-root"), DurabilityPolicy::Flush)?;
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
    let child = binding.branch_child(
        &opened.store,
        &ChildBranchRequest {
            child_session_id: Uuid::new_v4().to_string(),
            name_stem: "worker".to_owned(),
            kind: crate::session::events::ChildBranchKind::Spawn,
            durability: ChildDurability::Persist,
            model: "gpt-child".to_owned(),
            working_dir: "/w".to_owned(),
        },
    )?;
    let child_id = child
        .session_id
        .ok_or_else(|| std::io::Error::other("persistent child has no session id"))?;
    let child_entry = manager.resolve(&child_id)?;
    let child_rel_path = child_entry
        .rel_path
        .as_deref()
        .ok_or_else(|| std::io::Error::other("persistent child has no relative path"))?;
    let child_abs = manager.data_dir().join(child_rel_path);
    Ok((root_id, child_id, child_abs))
}

/// F3 cascade: deleting a root removes its file, its `{id}/`
/// directory (child timelines included), the child index rows, and
/// the root row — nothing phantom survives.
#[test]
fn delete_root_cascades_children_dir_and_rows() -> AdminTestResult {
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    let (root_id, child_id, child_abs) = root_with_child(&manager)?;
    assert!(child_abs.exists(), "fixture: child file on disk");

    manager.delete(&root_id)?;

    assert!(!child_abs.exists(), "child timeline deleted with the root");
    assert!(
        !tmp.path().join(&root_id).exists(),
        "the root's {{id}}/ directory is gone",
    );
    let rows = read_index(tmp.path())?;
    assert!(
        !rows.iter().any(|e| e.id == root_id || e.id == child_id),
        "no root or child row survives the cascade: {rows:?}",
    );
    Ok(())
}

/// F3 crash residue: a previous delete attempt that crashed AFTER
/// `remove_dir_all` but BEFORE the row sweep leaves phantom child
/// rows over deleted files. Re-running delete must still sweep them
/// — the sweep is never gated on the directory existing.
#[test]
fn delete_rerun_sweeps_phantom_child_rows_after_crash_residue() -> AdminTestResult {
    let tmp = tempfile::tempdir()?;
    let manager = SessionManager::new(tmp.path());
    let (root_id, child_id, _child_abs) = root_with_child(&manager)?;

    // Simulate the crash residue: the children directory is already
    // gone, but the child row (and the root) are still indexed.
    fs::remove_dir_all(tmp.path().join(&root_id))?;
    assert!(
        read_index(tmp.path())?.iter().any(|e| e.id == child_id),
        "fixture: the phantom child row is present",
    );

    manager.delete(&root_id)?;

    let rows = read_index(tmp.path())?;
    assert!(
        !rows.iter().any(|e| e.id == root_id || e.id == child_id),
        "the re-run must sweep the phantom child row: {rows:?}",
    );
    Ok(())
}
