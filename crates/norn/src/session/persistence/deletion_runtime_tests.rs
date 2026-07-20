use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;
use uuid::Uuid;

use super::index::{
    DeleteCheckpoint, delete_session_transaction, delete_session_transaction_with_hook,
    publish_new_child_session, publish_new_session, read_index,
};
use super::types::{
    ResumeFidelity, SESSION_FORMAT_VERSION, SessionIndexEntry, SessionPersistError,
    SessionRecordOrigin, SessionStatus,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn entry(id: &str) -> SessionIndexEntry {
    let now = Utc::now();
    SessionIndexEntry {
        id: id.to_owned(),
        generation: Uuid::new_v4(),
        name: None,
        model: "gpt-test".to_owned(),
        working_dir: "/workspace".to_owned(),
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
        fidelity: ResumeFidelity::Canonical,
        origin: SessionRecordOrigin::Native,
        provider_state_identity: None,
    }
}

fn child(root_id: &str, id: &str, parent: &SessionIndexEntry) -> SessionIndexEntry {
    SessionIndexEntry {
        rel_path: Some(format!("{root_id}/children/{id}.jsonl")),
        parent_id: Some(parent.id.clone()),
        ..entry(id)
    }
}

fn publish_root(data_dir: &Path, entry: &SessionIndexEntry) -> TestResult {
    publish_new_session(data_dir, entry, &[], None)?;
    Ok(())
}

fn publish_child(
    data_dir: &Path,
    entry: &SessionIndexEntry,
    parent: &SessionIndexEntry,
) -> TestResult {
    publish_new_child_session(data_dir, entry, &[], parent.generation, None)?;
    Ok(())
}

#[test]
fn deleting_child_removes_its_transitive_descendants_only() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = entry("cascade-root");
    let target = child(&root.id, "cascade-target", &root);
    let grandchild = child(&root.id, "cascade-grandchild", &target);
    let sibling = child(&root.id, "cascade-sibling", &root);
    publish_root(directory.path(), &root)?;
    publish_child(directory.path(), &target, &root)?;
    publish_child(directory.path(), &grandchild, &target)?;
    publish_child(directory.path(), &sibling, &root)?;

    let removed = delete_session_transaction(directory.path(), &target.id, None)?;

    assert_eq!(removed, target);
    assert_eq!(
        read_index(directory.path())?,
        vec![root.clone(), sibling.clone()]
    );
    assert!(timeline_path(directory.path(), &root).exists());
    assert!(timeline_path(directory.path(), &sibling).exists());
    assert!(!timeline_path(directory.path(), &target).exists());
    assert!(!timeline_path(directory.path(), &grandchild).exists());
    Ok(())
}

#[test]
fn cleanup_failure_after_index_publication_is_typed_and_recoverable() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = entry("cleanup-pending");
    publish_root(directory.path(), &indexed)?;
    let timeline = timeline_path(directory.path(), &indexed);

    let mut sabotage = |checkpoint| {
        if checkpoint == DeleteCheckpoint::IndexPublished {
            std::fs::remove_file(&timeline)?;
            std::fs::create_dir(&timeline)?;
        }
        Ok(())
    };
    let error =
        delete_session_transaction_with_hook(directory.path(), &indexed.id, None, &mut sabotage)
            .err()
            .ok_or_else(|| {
                io::Error::other("post-commit cleanup sabotage unexpectedly succeeded")
            })?;

    let transaction_id = match error {
        SessionPersistError::DeletionCleanupPending {
            id,
            transaction_id,
            source,
        } => {
            assert_eq!(id, indexed.id);
            assert!(matches!(*source, SessionPersistError::Io(_)));
            Uuid::parse_str(&transaction_id)?;
            transaction_id
        }
        other => {
            return Err(io::Error::other(format!(
                "post-commit cleanup returned the wrong error: {other}"
            ))
            .into());
        }
    };
    let raw_index = std::fs::read_to_string(directory.path().join("index.jsonl"))?;
    assert!(!raw_index.contains(&indexed.id));
    assert_eq!(
        deletion_journal_ids(directory.path())?,
        vec![transaction_id]
    );

    std::fs::remove_dir(&timeline)?;
    assert!(read_index(directory.path())?.is_empty());
    assert!(deletion_journal_ids(directory.path())?.is_empty());
    Ok(())
}

#[test]
fn partial_subtree_cleanup_recovers_after_one_timeline_was_removed() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = entry("partial-cleanup-root");
    let target = child(&root.id, "partial-cleanup-target", &root);
    let descendant = child(&root.id, "partial-cleanup-descendant", &target);
    publish_root(directory.path(), &root)?;
    publish_child(directory.path(), &target, &root)?;
    publish_child(directory.path(), &descendant, &target)?;
    let target_timeline = timeline_path(directory.path(), &target);
    let descendant_timeline = timeline_path(directory.path(), &descendant);

    let mut sabotage = |checkpoint| {
        if checkpoint == DeleteCheckpoint::IndexPublished {
            std::fs::remove_file(&descendant_timeline)?;
            std::fs::create_dir(&descendant_timeline)?;
        }
        Ok(())
    };
    let error =
        delete_session_transaction_with_hook(directory.path(), &target.id, None, &mut sabotage)
            .err()
            .ok_or_else(|| io::Error::other("partial cleanup sabotage unexpectedly succeeded"))?;
    assert!(matches!(
        error,
        SessionPersistError::DeletionCleanupPending { .. }
    ));
    assert!(!target_timeline.exists());
    assert!(descendant_timeline.is_dir());
    assert_eq!(deletion_journal_ids(directory.path())?.len(), 1);

    std::fs::remove_dir(&descendant_timeline)?;
    assert_eq!(read_index(directory.path())?, vec![root]);
    assert!(deletion_journal_ids(directory.path())?.is_empty());
    assert!(!descendant_timeline.exists());
    Ok(())
}

#[test]
fn postcommit_journal_must_reconstruct_a_valid_strict_index() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = entry("invalid-journal-generation");
    publish_root(directory.path(), &indexed)?;
    let timeline = timeline_path(directory.path(), &indexed);

    let mut corrupt = |checkpoint| {
        if checkpoint == DeleteCheckpoint::IndexPublished {
            replace_journal_generation(directory.path(), Uuid::nil())?;
            return Err(io::Error::other("injected stop after journal corruption").into());
        }
        Ok(())
    };
    let interrupted =
        delete_session_transaction_with_hook(directory.path(), &indexed.id, None, &mut corrupt);
    assert!(interrupted.is_err());
    assert!(timeline.exists());

    let recovery = read_index(directory.path())
        .err()
        .ok_or_else(|| io::Error::other("invalid journal generation was trusted for cleanup"))?;
    assert!(matches!(
        recovery,
        SessionPersistError::DeletionConflict { .. }
    ));
    assert!(timeline.exists());
    assert_eq!(deletion_journal_ids(directory.path())?.len(), 1);
    Ok(())
}

#[test]
fn noncanonical_deletion_uuid_name_is_foreign_and_untouched() -> TestResult {
    let directory = tempfile::tempdir()?;
    let indexed = entry("foreign-deletion-name");
    publish_root(directory.path(), &indexed)?;
    let foreign = directory
        .path()
        .join(".session-deletion.AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA.json");
    std::fs::write(&foreign, b"foreign")?;

    assert_eq!(read_index(directory.path())?, vec![indexed]);
    assert_eq!(std::fs::read(&foreign)?, b"foreign");
    Ok(())
}

fn timeline_path(data_dir: &Path, entry: &SessionIndexEntry) -> PathBuf {
    data_dir.join(
        entry
            .rel_path
            .clone()
            .unwrap_or_else(|| format!("{}.jsonl", entry.id)),
    )
}

fn deletion_journal_ids(data_dir: &Path) -> TestResult<Vec<String>> {
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(data_dir)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(id) = name
            .strip_prefix(".session-deletion.")
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        if Uuid::parse_str(id).is_ok() {
            ids.push(id.to_owned());
        }
    }
    ids.sort();
    Ok(ids)
}

fn replace_journal_generation(
    data_dir: &Path,
    generation: Uuid,
) -> Result<(), SessionPersistError> {
    let mut journals = Vec::new();
    for entry in std::fs::read_dir(data_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_str().is_some_and(|name| {
            name.strip_prefix(".session-deletion.")
                .and_then(|name| name.strip_suffix(".json"))
                .is_some_and(|id| Uuid::parse_str(id).is_ok())
        }) {
            journals.push(entry.path());
        }
    }
    if journals.len() != 1 {
        return Err(io::Error::other(format!(
            "expected one deletion journal, found {}",
            journals.len()
        ))
        .into());
    }
    let mut document: serde_json::Value =
        serde_json::from_reader(std::fs::File::open(&journals[0])?)?;
    let first = document
        .get_mut("removed")
        .and_then(serde_json::Value::as_array_mut)
        .and_then(|removed| removed.first_mut())
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| io::Error::other("deletion journal has no first removed row"))?;
    first.insert(
        "generation".to_owned(),
        serde_json::Value::String(generation.to_string()),
    );
    std::fs::write(&journals[0], serde_json::to_vec(&document)?)?;
    Ok(())
}
