use std::error::Error;
#[cfg(unix)]
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::Utc;

use crate::session::events::{EventBase, EventUsage, ProviderEpochBoundaryReason, SessionEvent};
use crate::session::persistence::read_index;
use crate::session::persistence::strict::ResumeFidelity;

use super::cutover::{CUTOVER_RECEIPT_FILE, CUTOVER_RECEIPT_VERSION, INITIAL_INDEX_FILE};
#[cfg(all(unix, not(target_vendor = "apple")))]
use super::export_legacy_session_raw;
use super::stage_ownership::{MARKER_FILE, STAGE_OWNERSHIP_VERSION};
use super::transaction::{BACKUP_STAGE, STRICT_STAGE};
use super::{
    LegacyClassificationReason, SessionMigrationError, SessionMigrationOutcome,
    migrate_legacy_sessions, read_legacy_migration_manifest, verify_legacy_session_migration,
};

const SESSION_ID: &str = "33333333-3333-4333-8333-333333333333";

#[cfg(unix)]
#[test]
fn non_utf8_selector_codec_is_byte_reversible() -> Result<(), Box<dyn Error>> {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

    let path = PathBuf::from(OsString::from_vec(b"orphan-\xff.jsonl".to_vec()));
    let selector = super::classify::encode_relative_path(&path)?;
    let decoded = super::classify::decode_relative_path(&selector)?;
    assert!(selector.starts_with("unix-path-hex:"));
    assert_eq!(decoded.as_os_str().as_bytes(), path.as_os_str().as_bytes());
    Ok(())
}

#[test]
fn literal_reserved_selector_prefix_is_reversible() -> Result<(), Box<dyn Error>> {
    for literal in ["unix-path-hex:literal.jsonl", "utf8-path-hex:literal.jsonl"] {
        let path = PathBuf::from(literal);
        let selector = super::classify::encode_relative_path(&path)?;
        let decoded = super::classify::decode_relative_path(&selector)?;
        assert!(selector.starts_with("utf8-path-hex:"));
        assert_eq!(decoded, path);
    }
    Ok(())
}

#[test]
fn published_receipt_verifies_and_old_writer_divergence_fails_closed() -> Result<(), Box<dyn Error>>
{
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    write_empty_legacy_store(&root)?;
    let _ = migrate_legacy_sessions(&root)?;

    let verified = verify_legacy_session_migration(&root)?;
    fs::write(root.join("sessions/late-old-writer.jsonl"), b"late write\n")?;
    let error = verify_legacy_session_migration(&root)
        .err()
        .ok_or_else(|| io::Error::other("diverged legacy source unexpectedly verified"))?;

    assert!(matches!(
        error,
        SessionMigrationError::LegacySourceDiverged {
            published_sha256,
            ..
        } if published_sha256 == verified.source_tree_sha256
    ));
    Ok(())
}

#[test]
fn published_ownership_receipts_are_exact_and_manifest_versioned() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    write_empty_legacy_store(&root)?;
    let outcome = migrate_legacy_sessions(&root)?;
    let (digest, destination, backup) = migrated_paths(outcome)?;
    let manifest = read_legacy_migration_manifest(&root)?;

    assert_eq!(manifest.stage_ownership_version, STAGE_OWNERSHIP_VERSION);
    assert_eq!(manifest.cutover_receipt_version, CUTOVER_RECEIPT_VERSION);
    assert!(destination.join(CUTOVER_RECEIPT_FILE).is_file());
    assert!(destination.join(INITIAL_INDEX_FILE).is_file());
    assert_eq!(
        fs::read_to_string(destination.join(MARKER_FILE))?,
        format!("norn-session-migration-stage-v1\nstrict-store\n{digest}\n")
    );
    let backup_container = backup
        .parent()
        .ok_or_else(|| io::Error::other("published backup has no container"))?;
    assert_eq!(
        fs::read_to_string(backup_container.join(MARKER_FILE))?,
        format!("norn-session-migration-stage-v1\nbackup\n{digest}\n")
    );
    fs::write(
        destination.join("index.jsonl"),
        b" { \"norn_session_format\" : 2 }\n",
    )?;
    super::verify_legacy_session_cutover(&root)?;
    let _ = verify_legacy_session_migration(&root)?;
    Ok(())
}

#[test]
fn bounded_cutover_rejects_missing_incomplete_version_and_marker_receipts()
-> Result<(), Box<dyn Error>> {
    for replacement in [None, Some(b"{}\n".as_slice())] {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        write_empty_legacy_store(&root)?;
        let (_, destination, _) = migrated_paths(migrate_legacy_sessions(&root)?)?;
        assert!(super::verify_legacy_session_cutover(&root).is_ok());
        let receipt = destination.join(CUTOVER_RECEIPT_FILE);
        match replacement {
            Some(bytes) => fs::write(receipt, bytes)?,
            None => fs::remove_file(receipt)?,
        }
        assert!(super::verify_legacy_session_cutover(&root).is_err());
    }

    for target in ["receipt-version", "ownership-marker"] {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        write_empty_legacy_store(&root)?;
        let (_, destination, _) = migrated_paths(migrate_legacy_sessions(&root)?)?;
        if target == "receipt-version" {
            let receipt_path = destination.join(CUTOVER_RECEIPT_FILE);
            let receipt = fs::read_to_string(&receipt_path)?;
            fs::write(
                receipt_path,
                receipt.replacen("\"receipt_version\":1", "\"receipt_version\":2", 1),
            )?;
        } else {
            fs::write(
                destination.join(MARKER_FILE),
                b"norn-session-migration-stage-v1\nbackup\n0000000000000000000000000000000000000000000000000000000000000000\n",
            )?;
        }
        assert!(super::verify_legacy_session_cutover(&root).is_err());
    }
    Ok(())
}

#[test]
fn deep_verify_rejects_changed_immutable_publication_evidence() -> Result<(), Box<dyn Error>> {
    for target in [INITIAL_INDEX_FILE, super::MIGRATION_MANIFEST_FILE] {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        write_empty_legacy_store(&root)?;
        let (_, destination, _) = migrated_paths(migrate_legacy_sessions(&root)?)?;
        let path = destination.join(target);
        let mut bytes = fs::read(&path)?;
        bytes.push(b' ');
        fs::write(path, bytes)?;

        super::verify_legacy_session_cutover(&root)?;
        assert!(verify_legacy_session_migration(&root).is_err());
    }
    Ok(())
}

#[test]
fn unowned_fixed_stage_directories_are_preserved() -> Result<(), Box<dyn Error>> {
    for stage in [BACKUP_STAGE, STRICT_STAGE] {
        let temp = tempfile::tempdir()?;
        let root = temp.path().canonicalize()?;
        write_empty_legacy_store(&root)?;
        let stage_path = root.join(stage);
        fs::create_dir(&stage_path)?;
        fs::write(stage_path.join("foreign"), b"must survive")?;

        let error = migrate_legacy_sessions(&root)
            .err()
            .ok_or_else(|| io::Error::other("unowned fixed stage unexpectedly accepted"))?;

        assert!(matches!(
            error,
            SessionMigrationError::StageOwnershipConflict { .. }
        ));
        assert_eq!(fs::read(stage_path.join("foreign"))?, b"must survive");
    }
    Ok(())
}

#[cfg(all(unix, not(target_vendor = "apple")))]
#[test]
fn non_utf8_orphan_selector_round_trips_for_exact_export() -> Result<(), Box<dyn Error>> {
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    write_empty_legacy_store(&root)?;
    let name = OsString::from_vec(b"orphan-\xff.jsonl".to_vec());
    let payload = b"non-UTF8 orphan bytes\n";
    fs::write(root.join("sessions").join(&name), payload)?;

    let _ = migrate_legacy_sessions(&root)?;
    let manifest = read_legacy_migration_manifest(&root)?;
    let record = manifest
        .sessions
        .iter()
        .find(|record| {
            record
                .reasons
                .contains(&LegacyClassificationReason::OrphanTimeline)
        })
        .ok_or_else(|| io::Error::other("non-UTF8 orphan was not catalogued"))?;
    let selector = record
        .source_path
        .as_deref()
        .ok_or_else(|| io::Error::other("orphan record has no source selector"))?;
    assert!(selector.starts_with("unix-path-hex:"));
    assert!(selector.contains(&hex_byte_string(name.as_bytes())));
    let catalog_id = record
        .catalog_id
        .as_deref()
        .ok_or_else(|| io::Error::other("orphan record has no catalog id"))?;
    let mut exported = Vec::new();
    let exported_record = export_legacy_session_raw(&root, catalog_id, &mut exported)?;
    assert_eq!(&exported_record, record);
    assert_eq!(exported, payload);
    Ok(())
}

#[test]
fn legacy_provider_epoch_boundary_is_inspect_only_and_cannot_preserve_anchor()
-> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let boundary = SessionEvent::ProviderEpochBoundary {
        base: EventBase::new(None),
        reason: ProviderEpochBoundaryReason::MigratedLegacy,
    };
    let assistant = SessionEvent::AssistantMessage {
        base: EventBase::new(Some(boundary.base().id.clone())),
        response_items: Vec::new(),
        content: "spoofed anchor".to_owned(),
        thinking: String::new(),
        reasoning: Vec::new(),
        tool_calls: Vec::new(),
        usage: EventUsage::default(),
        stop_reason: "end_turn".to_owned(),
        response_id: Some("resp_must_not_survive".to_owned()),
    };
    let timeline = legacy_timeline(&[boundary, assistant])?;
    write_indexed_legacy_store(&root, &timeline)?;

    let outcome = migrate_legacy_sessions(&root)?;
    let (_, destination, _) = migrated_paths(outcome)?;
    let manifest = read_legacy_migration_manifest(&root)?;
    let record = manifest
        .sessions
        .first()
        .ok_or_else(|| io::Error::other("migration manifest has no session"))?;
    assert_eq!(record.fidelity, ResumeFidelity::InspectOnly);
    assert!(
        record
            .reasons
            .contains(&LegacyClassificationReason::SpoofedProviderEpochBoundary)
    );
    assert!(read_index(&destination)?.is_empty());
    assert!(!destination.join(format!("{SESSION_ID}.jsonl")).exists());
    Ok(())
}

fn write_empty_legacy_store(root: &Path) -> Result<(), Box<dyn Error>> {
    let sessions = root.join("sessions");
    fs::create_dir(&sessions)?;
    fs::write(sessions.join("index.jsonl"), b"")?;
    Ok(())
}

fn write_indexed_legacy_store(root: &Path, timeline: &[u8]) -> Result<(), Box<dyn Error>> {
    let sessions = root.join("sessions");
    fs::create_dir(&sessions)?;
    let now = Utc::now();
    let row = serde_json::json!({
        "id": SESSION_ID,
        "name": "spoof fixture",
        "model": "fixture-model",
        "working_dir": "/fixture/workspace",
        "created_at": now,
        "updated_at": now,
        "event_count": 2,
        "status": "completed",
        "format_version": 1,
        "total_input_tokens": 0,
        "total_output_tokens": 0,
        "total_cache_read_tokens": 0,
        "rel_path": null,
        "parent_id": null,
    });
    let mut index = serde_json::to_vec(&row)?;
    index.push(b'\n');
    fs::write(sessions.join("index.jsonl"), index)?;
    fs::write(sessions.join(format!("{SESSION_ID}.jsonl")), timeline)?;
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

#[cfg(all(unix, not(target_vendor = "apple")))]
fn hex_byte_string(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
