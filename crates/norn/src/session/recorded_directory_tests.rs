//! Disk-backed directory coverage, resume, subtree boundaries and stale-binding refusal.

use std::sync::Arc;

use super::{RecordedSessionDirectory, SessionBinding};
use crate::session::branch::{ChildBranchRequest, ChildDurability, SessionBrancher};
use crate::session::events::ChildBranchKind;
use crate::session::persistence::types::SessionPersistError;
use crate::session::store::DurabilityPolicy;
use crate::session::{CreateSessionOptions, SessionManager};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn request(id: &str) -> ChildBranchRequest {
    ChildBranchRequest {
        child_session_id: id.into(),
        name_stem: id.into(),
        kind: ChildBranchKind::Spawn,
        durability: ChildDurability::Persist,
        model: "fixture".into(),
        working_dir: "/fixture".into(),
    }
}

fn ids(directory: &RecordedSessionDirectory) -> Vec<&str> {
    match directory {
        RecordedSessionDirectory::Ephemeral => Vec::new(),
        RecordedSessionDirectory::Registered { sessions } => {
            sessions.iter().map(|row| row.session_id.as_str()).collect()
        }
    }
}

#[test]
fn resumed_bindings_discover_only_registered_subtree_without_reading_children() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let options = || CreateSessionOptions {
        model: "fixture".into(),
        working_dir: "/fixture".into(),
        name: None,
    };
    let opened = manager.create(options(), DurabilityPolicy::Flush)?;
    let root_id = opened.entry.id.clone();
    let brancher = Arc::new(SessionBrancher::new(
        manager.clone(),
        root_id.clone(),
        DurabilityPolicy::Flush,
    ));
    let root = SessionBinding::persistent_root(Arc::clone(&brancher), &opened.entry, &[]);
    let child = root.branch_child(&opened.store, &request("first-child"))?;
    let sibling = root.branch_child(&opened.store, &request("second-child"))?;
    let grandchild = child
        .binding
        .branch_child(&child.store, &request("grandchild"))?;
    let foreign = manager.create(options(), DurabilityPolicy::Flush)?;
    let mut ephemeral = request("ephemeral");
    ephemeral.durability = ChildDurability::Ephemeral;
    let transient = root.branch_child(&opened.store, &ephemeral)?;
    assert_eq!(
        transient.binding.recorded_directory()?,
        RecordedSessionDirectory::Ephemeral
    );
    drop((child, sibling, grandchild, foreign, transient, opened));

    let resumed = manager.resume(&root_id, DurabilityPolicy::Flush)?;
    let root = SessionBinding::persistent_root(
        Arc::clone(&brancher),
        &resumed.entry,
        &resumed.store.events(),
    );
    let reopened_child = manager.resume("first-child", DurabilityPolicy::Flush)?;
    let child = SessionBinding::persistent_root(
        brancher,
        &reopened_child.entry,
        &reopened_child.store.events(),
    );
    drop((resumed, reopened_child));
    let child_row = manager.resolve("first-child")?;
    let child_path = temp
        .path()
        .join(child_row.rel_path.ok_or("child has no path")?);
    // A deliberately unreadable child proves discovery does not eagerly decode it.
    let original = std::fs::read(&child_path)?;
    std::fs::write(&child_path, b"not a session timeline\n")?;
    let root_path = temp.path().join(format!("{root_id}.jsonl"));
    let root_bytes = std::fs::read(&root_path)?;
    let index_path = temp.path().join("index.jsonl");
    let index_bytes = std::fs::read(&index_path)?;
    let directory = root.recorded_directory()?;
    assert_eq!(
        ids(&directory),
        [
            root_id.as_str(),
            "first-child",
            "grandchild",
            "second-child"
        ]
    );
    let child_directory = child.recorded_directory()?;
    assert_eq!(ids(&child_directory), ["first-child", "grandchild"]);
    let RecordedSessionDirectory::Registered { sessions } = child_directory else {
        return Err("resumed child lost its persistent directory".into());
    };
    assert_eq!(sessions[0].parent_session_id, None);
    assert_eq!(
        sessions[1].parent_session_id.as_deref(),
        Some("first-child")
    );
    assert_eq!(
        sessions[0].generation,
        manager.resolve("first-child")?.generation
    );
    assert_eq!(std::fs::read(&child_path)?, b"not a session timeline\n");
    assert_eq!(std::fs::read(&root_path)?, root_bytes);
    assert_eq!(std::fs::read(&index_path)?, index_bytes);
    std::fs::remove_file(&child_path)?;
    assert_eq!(root.recorded_directory()?, directory);
    std::fs::write(child_path, original)?;
    Ok(())
}

#[test]
fn replaced_or_removed_registration_cannot_be_read_through_old_binding() -> TestResult {
    let temp = tempfile::tempdir()?;
    let manager = SessionManager::new(temp.path());
    let opened = manager.create(
        CreateSessionOptions {
            model: "fixture".into(),
            working_dir: "/fixture".into(),
            name: None,
        },
        DurabilityPolicy::Flush,
    )?;
    let id = opened.entry.id.clone();
    let brancher = Arc::new(SessionBrancher::new(
        manager.clone(),
        id.clone(),
        DurabilityPolicy::Flush,
    ));
    let mut replacement = opened.entry.clone();
    replacement.generation = uuid::Uuid::new_v4();
    let stale = SessionBinding::persistent_root(Arc::clone(&brancher), &replacement, &[]);
    assert!(
        matches!(stale.recorded_directory(), Err(SessionPersistError::GenerationChanged { id: actual }) if actual == id)
    );
    let valid = SessionBinding::persistent_root(brancher, &opened.entry, &[]);
    drop(opened);
    manager.delete(&id)?;
    assert!(
        matches!(valid.recorded_directory(), Err(SessionPersistError::GenerationChanged { id: actual }) if actual == id)
    );
    Ok(())
}
