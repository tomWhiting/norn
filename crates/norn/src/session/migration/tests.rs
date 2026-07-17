use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::session::events::{EventBase, EventUsage, SessionEvent};
use crate::session::persistence::strict::{ResumeFidelity, SessionRecordOrigin};
use crate::session::persistence::{
    SESSION_FORMAT_VERSION, read_index, read_session_events, read_session_events_for_entry,
};

use super::stage_ownership::{StageKind, replace_owned_stage};
use super::transaction::{
    BACKUP_STAGE, MigrationCheckpoint, STRICT_STAGE, migrate_legacy_sessions_with_hook,
};
use super::types::{LegacyClassificationReason, SessionMigrationManifest, SessionMigrationOutcome};
use super::{
    MIGRATION_MANIFEST_FILE, export_legacy_session_raw, migrate_legacy_sessions,
    read_legacy_migration_manifest,
};
use crate::util::PrivateRoot;

const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";
const CHILD_SESSION_ID: &str = "22222222-2222-4222-8222-222222222222";

#[test]
fn rejects_relative_root_before_mutation() -> Result<(), Box<dyn Error>> {
    let error = migrate_legacy_sessions(Path::new("relative-norn-root"))
        .err()
        .ok_or_else(|| io::Error::other("relative root unexpectedly accepted"))?;

    assert!(matches!(
        error,
        super::SessionMigrationError::InvalidNornRoot { .. }
    ));
    Ok(())
}

#[test]
fn header_only_timeline_is_fresh_epoch_and_idempotent() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let timeline = b"{\"norn_session_format\":1}\n";
    write_legacy_fixture(&root, timeline, 9)?;
    let index_before = fs::read(root.join("sessions/index.jsonl"))?;
    let timeline_before = fs::read(root.join(format!("sessions/{SESSION_ID}.jsonl")))?;

    let first = migrate_legacy_sessions(&root)?;
    let (digest, destination, backup) = migrated_paths(first)?;
    let manifest = read_manifest(&destination)?;

    assert_eq!(manifest.source_tree_sha256, digest);
    assert_eq!(manifest.sessions.len(), 1);
    assert_eq!(
        manifest.sessions[0].fidelity,
        ResumeFidelity::FreshEpochProjection
    );
    assert!(manifest.sessions[0].catalog_id.is_none());
    assert!(
        manifest.sessions[0]
            .reasons
            .contains(&LegacyClassificationReason::EmptyTimeline)
    );
    assert!(
        manifest.sessions[0]
            .reasons
            .contains(&LegacyClassificationReason::StaleIndexMetadata)
    );
    assert_eq!(fs::read(root.join("sessions/index.jsonl"))?, index_before);
    assert_eq!(
        fs::read(root.join(format!("sessions/{SESSION_ID}.jsonl")))?,
        timeline_before
    );
    assert_eq!(fs::read(backup.join("index.jsonl"))?, index_before);
    assert_eq!(
        fs::read(backup.join(format!("{SESSION_ID}.jsonl")))?,
        timeline_before
    );

    let second = migrate_legacy_sessions(&root)?;
    assert!(matches!(
        second,
        SessionMigrationOutcome::AlreadyMigrated {
            source_tree_sha256,
            ..
        } if source_tree_sha256 == digest
    ));
    Ok(())
}

#[test]
fn canonical_history_is_rewritten_to_strict_format_without_source_mutation()
-> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let timeline = legacy_timeline(&[SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "canonical legacy user message".to_owned(),
    }])?;
    write_legacy_fixture(&root, &timeline, 1)?;
    let source_before = fs::read(root.join(format!("sessions/{SESSION_ID}.jsonl")))?;

    let outcome = migrate_legacy_sessions(&root)?;
    let counts = outcome_counts(&outcome);
    let (_, destination, _) = migrated_paths(outcome)?;
    assert_eq!(counts.canonical, 1);
    assert_eq!(counts.fresh_epoch_projection, 0);
    assert_eq!(counts.inspect_only, 0);

    let entries = read_index(&destination)?;
    assert_eq!(entries.len(), 1);
    let entry = entries
        .first()
        .ok_or_else(|| io::Error::other("strict index has no migrated row"))?;
    assert_eq!(entry.format_version, SESSION_FORMAT_VERSION);
    assert_eq!(entry.fidelity, ResumeFidelity::Canonical);
    match &entry.origin {
        SessionRecordOrigin::MigratedLegacy {
            source_format,
            source_sha256,
        } => {
            assert_eq!(*source_format, 1);
            assert_eq!(source_sha256.len(), 64);
        }
        SessionRecordOrigin::Native => {
            return Err(io::Error::other("migrated row was marked native").into());
        }
    }
    let replay = read_session_events(&destination, SESSION_ID)?;
    assert_eq!(replay.format_version, Some(SESSION_FORMAT_VERSION));
    let Some(SessionEvent::UserMessage { content, .. }) = replay.events.first() else {
        return Err(io::Error::other("migrated canonical event changed type").into());
    };
    assert_eq!(content, "canonical legacy user message");
    assert_eq!(
        fs::read(root.join(format!("sessions/{SESSION_ID}.jsonl")))?,
        source_before
    );
    Ok(())
}

#[test]
fn flattened_assistant_history_is_explicit_fresh_epoch_without_fabrication()
-> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let timeline = legacy_timeline(&[SessionEvent::AssistantMessage {
        base: EventBase::new(None),
        response_items: Vec::new(),
        content: "flattened answer".to_owned(),
        thinking: "legacy projection".to_owned(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_legacy_anchor".to_owned()),
    }])?;
    write_legacy_fixture(&root, &timeline, 1)?;

    let (_, destination, _) = migrated_paths(migrate_legacy_sessions(&root)?)?;
    let manifest = read_manifest(&destination)?;
    let record = manifest
        .sessions
        .first()
        .ok_or_else(|| io::Error::other("migration manifest has no record"))?;
    assert_eq!(record.fidelity, ResumeFidelity::FreshEpochProjection);
    assert!(
        record
            .reasons
            .contains(&LegacyClassificationReason::FlattenedAssistantTurn)
    );

    let replay = read_session_events(&destination, SESSION_ID)?;
    let Some(SessionEvent::AssistantMessage {
        response_items,
        content,
        response_id,
        ..
    }) = replay.events.first()
    else {
        return Err(io::Error::other("migrated flattened event changed type").into());
    };
    assert!(response_items.is_empty());
    assert_eq!(content, "flattened answer");
    assert_eq!(response_id.as_deref(), Some("resp_legacy_anchor"));
    Ok(())
}

#[test]
fn nested_history_and_resumable_auxiliary_artifacts_migrate_together() -> Result<(), Box<dyn Error>>
{
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let sessions = root.join("sessions");
    fs::create_dir_all(sessions.join(format!("{SESSION_ID}/children")))?;
    fs::create_dir_all(sessions.join(format!("{SESSION_ID}/spool")))?;
    fs::create_dir_all(sessions.join(format!("{SESSION_ID}/artifacts")))?;
    let root_timeline = legacy_timeline(&[SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "root".to_owned(),
    }])?;
    let child_timeline = legacy_timeline(&[SessionEvent::UserMessage {
        base: EventBase::new(None),
        content: "child".to_owned(),
    }])?;
    fs::write(sessions.join(format!("{SESSION_ID}.jsonl")), root_timeline)?;
    fs::write(
        sessions.join(format!("{SESSION_ID}/children/{CHILD_SESSION_ID}.jsonl")),
        child_timeline,
    )?;
    fs::write(
        sessions.join(format!("{SESSION_ID}/spool/output.bin")),
        b"verbatim output",
    )?;
    fs::write(
        sessions.join(format!("{SESSION_ID}/artifacts/fetched.bin")),
        b"fetched document",
    )?;
    write_legacy_index_rows(
        &sessions,
        &[
            legacy_index_row(SESSION_ID, None, None, 1),
            legacy_index_row(
                CHILD_SESSION_ID,
                Some(&format!("{SESSION_ID}/children/{CHILD_SESSION_ID}.jsonl")),
                Some(SESSION_ID),
                1,
            ),
        ],
    )?;

    let (_, destination, _) = migrated_paths(migrate_legacy_sessions(&root)?)?;
    let entries = read_index(&destination)?;
    assert_eq!(entries.len(), 2);
    let child = entries
        .iter()
        .find(|entry| entry.id == CHILD_SESSION_ID)
        .ok_or_else(|| io::Error::other("strict index has no migrated child"))?;
    assert_eq!(child.parent_id.as_deref(), Some(SESSION_ID));
    assert_eq!(
        fs::read(destination.join(format!("{SESSION_ID}/spool/output.bin")))?,
        b"verbatim output"
    );
    assert_eq!(
        fs::read(destination.join(format!("{SESSION_ID}/artifacts/fetched.bin")))?,
        b"fetched document"
    );
    let child_replay = read_session_events_for_entry(&destination, child)?;
    assert_eq!(child_replay.events.len(), 1);
    Ok(())
}

#[test]
fn preexisting_destination_fails_before_backup_or_stage_publication() -> Result<(), Box<dyn Error>>
{
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    write_legacy_fixture(&root, b"{\"norn_session_format\":1}\n", 0)?;
    let source_before = fs::read(root.join("sessions/index.jsonl"))?;
    fs::create_dir(root.join("session-store"))?;
    fs::write(root.join("session-store/foreign"), b"do not replace")?;

    let error = migrate_legacy_sessions(&root)
        .err()
        .ok_or_else(|| io::Error::other("preexisting destination was replaced"))?;
    assert!(matches!(
        error,
        super::SessionMigrationError::DestinationConflict { .. }
    ));
    assert_eq!(fs::read(root.join("sessions/index.jsonl"))?, source_before);
    assert_eq!(
        fs::read(root.join("session-store/foreign"))?,
        b"do not replace"
    );
    assert!(!root.join("session-migration-backups").exists());
    assert!(!root.join(BACKUP_STAGE).exists());
    assert!(!root.join(STRICT_STAGE).exists());
    Ok(())
}

#[test]
fn inspect_only_payload_lives_only_in_backup_and_manifest() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let invalid =
        b"{\"norn_session_format\":1}\n{\"type\":\"UserMessage\",\"type\":\"UserMessage\"}\n";
    write_legacy_fixture(&root, invalid, 1)?;

    let (_, destination, backup) = migrated_paths(migrate_legacy_sessions(&root)?)?;
    let manifest = read_manifest(&destination)?;
    let record = manifest
        .sessions
        .first()
        .ok_or_else(|| io::Error::other("migration manifest has no record"))?;

    assert_eq!(record.fidelity, ResumeFidelity::InspectOnly);
    assert!(record.catalog_id.is_some());
    assert!(record.destination_path.is_none());
    assert!(!destination.join(format!("{SESSION_ID}.jsonl")).exists());
    assert_eq!(
        fs::read(backup.join(format!("{SESSION_ID}.jsonl")))?,
        invalid
    );

    let published = read_legacy_migration_manifest(&root)?;
    assert_eq!(published, manifest);
    let catalog_id = record
        .catalog_id
        .as_deref()
        .ok_or_else(|| io::Error::other("inspect-only record lacks a catalog id"))?;
    let mut exported = Vec::new();
    let exported_record = export_legacy_session_raw(&root, catalog_id, &mut exported)?;
    assert_eq!(&exported_record, record);
    assert_eq!(exported, invalid);
    Ok(())
}

#[test]
fn unknown_legacy_catalog_id_writes_nothing() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let invalid = b"{\"norn_session_format\":1}\n{\"type\":\"unknown\"}\n";
    write_legacy_fixture(&root, invalid, 1)?;
    let _ = migrate_legacy_sessions(&root)?;

    let mut output = Vec::new();
    let error = export_legacy_session_raw(&root, "legacy-does-not-exist", &mut output)
        .err()
        .ok_or_else(|| io::Error::other("unknown legacy catalog id unexpectedly exported"))?;
    assert!(matches!(
        error,
        super::SessionMigrationError::LegacyCatalogNotFound { .. }
    ));
    assert!(output.is_empty());
    Ok(())
}

#[test]
fn changed_source_after_interruption_replaces_stable_stages() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    write_legacy_fixture(&root, b"{\"norn_session_format\":1}\n", 0)?;

    let backup_stage = root.join(BACKUP_STAGE);
    let strict_stage = root.join(STRICT_STAGE);
    let private_root = PrivateRoot::open(&root)?;
    let interrupted_digest = "0".repeat(64);
    replace_owned_stage(
        &private_root,
        &root,
        Path::new(BACKUP_STAGE),
        StageKind::Backup,
        &interrupted_digest,
    )?;
    replace_owned_stage(
        &private_root,
        &root,
        Path::new(STRICT_STAGE),
        StageKind::StrictStore,
        &interrupted_digest,
    )?;
    fs::write(backup_stage.join("old-source"), b"superseded digest")?;
    fs::write(strict_stage.join("old-source"), b"superseded digest")?;

    let (_, destination, backup) = migrated_paths(migrate_legacy_sessions(&root)?)?;

    assert!(!backup_stage.exists());
    assert!(!strict_stage.exists());
    assert!(destination.join(MIGRATION_MANIFEST_FILE).is_file());
    assert_eq!(
        fs::read(backup.join(format!("{SESSION_ID}.jsonl")))?,
        b"{\"norn_session_format\":1}\n"
    );
    Ok(())
}

include!("tests/recovery.rs");

fn write_legacy_fixture(
    root: &Path,
    timeline: &[u8],
    indexed_event_count: u64,
) -> Result<(), Box<dyn Error>> {
    let sessions = root.join("sessions");
    fs::create_dir(&sessions)?;
    let entry = legacy_index_row(SESSION_ID, None, None, indexed_event_count);
    write_legacy_index_rows(&sessions, &[entry])?;
    fs::write(sessions.join(format!("{SESSION_ID}.jsonl")), timeline)?;
    Ok(())
}

fn legacy_index_row(
    id: &str,
    rel_path: Option<&str>,
    parent_id: Option<&str>,
    event_count: u64,
) -> serde_json::Value {
    let now = Utc::now();
    serde_json::json!({
        "id": id,
        "name": "migration fixture",
        "model": "fixture-model",
        "working_dir": "/fixture/workspace",
        "created_at": now,
        "updated_at": now,
        "event_count": event_count,
        "status": "completed",
        "format_version": 1,
        "total_input_tokens": 0,
        "total_output_tokens": 0,
        "total_cache_read_tokens": 0,
        "rel_path": rel_path,
        "parent_id": parent_id,
    })
}

fn write_legacy_index_rows(
    sessions: &Path,
    rows: &[serde_json::Value],
) -> Result<(), Box<dyn Error>> {
    let mut index = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut index, row)?;
        index.push(b'\n');
    }
    fs::write(sessions.join("index.jsonl"), index)?;
    Ok(())
}

fn legacy_timeline(events: &[SessionEvent]) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut timeline = b"{\"norn_session_format\":1}\n".to_vec();
    for event in events {
        serde_json::to_writer(&mut timeline, event)?;
        timeline.push(b'\n');
    }
    Ok(timeline)
}

fn outcome_counts(outcome: &SessionMigrationOutcome) -> super::MigrationCounts {
    match outcome {
        SessionMigrationOutcome::Migrated { counts, .. }
        | SessionMigrationOutcome::AlreadyMigrated { counts, .. } => *counts,
    }
}

fn migrated_paths(
    outcome: SessionMigrationOutcome,
) -> Result<(String, PathBuf, PathBuf), Box<dyn Error>> {
    match outcome {
        SessionMigrationOutcome::Migrated {
            source_tree_sha256,
            destination,
            backup,
            ..
        } => Ok((source_tree_sha256, destination, backup)),
        SessionMigrationOutcome::AlreadyMigrated { .. } => {
            Err(io::Error::other("expected a newly migrated store").into())
        }
    }
}

fn read_manifest(destination: &Path) -> Result<SessionMigrationManifest, Box<dyn Error>> {
    let bytes = fs::read(destination.join(MIGRATION_MANIFEST_FILE))?;
    Ok(serde_json::from_slice(&bytes)?)
}
