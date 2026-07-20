use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufWriter, Write as _};
use std::path::{Component, Path};

use uuid::Uuid;

use crate::session::events::SessionEvent;
use crate::session::persistence::strict::{
    STRICT_SESSION_FORMAT_VERSION, SessionIndexEntry, SessionRecordOrigin, StrictFormatHeader,
};
use crate::util::{PrivateRoot, PrivateRootReader, PrivateTreeEntry};

use super::classify::{
    ClassifiedLegacyStore, ClassifiedSession, TimelineTotals, visit_legacy_events,
};
use super::error::SessionMigrationError;
use super::tree::copy_one_file;
use super::types::{MIGRATION_MANIFEST_FILE, SessionMigrationManifest};

const INDEX_FILE: &str = "index.jsonl";

pub(super) fn build_strict_stage(
    backup: &PrivateRootReader,
    backup_tree: &[PrivateTreeEntry],
    classified: &ClassifiedLegacyStore,
    root: &PrivateRoot,
    stage: &Path,
    manifest: &SessionMigrationManifest,
) -> Result<(), SessionMigrationError> {
    root.create_dir_all(stage).map_err(|error| {
        SessionMigrationError::mutation("creating strict migration stage", stage, error)
    })?;
    let mut index = Vec::with_capacity(classified.sessions.len());
    for session in &classified.sessions {
        write_strict_timeline(backup, session, root, stage)?;
        index.push(strict_index_entry(session));
    }
    index.sort_by(|left, right| left.id.cmp(&right.id));
    write_jsonl(root, &stage.join(INDEX_FILE), &index)?;

    write_json(root, &stage.join(MIGRATION_MANIFEST_FILE), manifest)?;
    copy_resumable_auxiliary_files(backup, backup_tree, classified, root, stage)?;
    root.sync_dir(stage).map_err(|error| {
        SessionMigrationError::mutation("synchronizing strict migration stage", stage, error)
    })?;
    Ok(())
}

fn write_strict_timeline(
    backup: &PrivateRootReader,
    session: &ClassifiedSession,
    root: &PrivateRoot,
    stage: &Path,
) -> Result<(), SessionMigrationError> {
    let destination = stage.join(&session.source_path);
    if let Some(parent) = destination.parent() {
        root.create_dir_all(parent).map_err(|error| {
            SessionMigrationError::mutation("creating strict timeline parent", parent, error)
        })?;
    }
    let file = root.create_new(&destination).map_err(|error| {
        SessionMigrationError::mutation("creating strict timeline", &destination, error)
    })?;
    let mut writer = BufWriter::new(file);
    write_row(&mut writer, &StrictFormatHeader::current(), &destination)?;
    let totals = visit_legacy_events(
        backup,
        &session.source_path,
        session.source_format,
        |event| write_event(&mut writer, &event, &destination),
    )?;
    require_same_totals(session.totals, totals, &session.entry.id)?;
    finish_file(writer, &destination)?;
    if let Some(parent) = destination.parent() {
        root.sync_dir(parent).map_err(|error| {
            SessionMigrationError::mutation("synchronizing strict timeline parent", parent, error)
        })?;
    }
    Ok(())
}

fn write_event(
    writer: &mut BufWriter<std::fs::File>,
    event: &SessionEvent,
    path: &Path,
) -> Result<(), SessionMigrationError> {
    write_row(writer, event, path)
}

fn strict_index_entry(session: &ClassifiedSession) -> SessionIndexEntry {
    SessionIndexEntry {
        id: session.entry.id.clone(),
        generation: Uuid::new_v4(),
        name: session.entry.name.clone(),
        model: session.entry.model.clone(),
        working_dir: session.entry.working_dir.clone(),
        created_at: session.entry.created_at,
        updated_at: session.entry.updated_at,
        event_count: session.totals.event_count,
        status: session.entry.status,
        format_version: STRICT_SESSION_FORMAT_VERSION,
        total_input_tokens: session.totals.input_tokens,
        total_output_tokens: session.totals.output_tokens,
        total_cache_read_tokens: session.totals.cache_read_tokens,
        rel_path: session.entry.rel_path.clone(),
        parent_id: session.entry.parent_id.clone(),
        fidelity: session.fidelity,
        origin: SessionRecordOrigin::MigratedLegacy {
            source_format: session.source_format,
            source_sha256: session.source_sha256.clone(),
        },
        provider_state_identity: None,
    }
}

fn copy_resumable_auxiliary_files(
    backup: &PrivateRootReader,
    backup_tree: &[PrivateTreeEntry],
    classified: &ClassifiedLegacyStore,
    root: &PrivateRoot,
    stage: &Path,
) -> Result<(), SessionMigrationError> {
    let root_ids = resumable_root_ids(&classified.sessions);
    let lengths = backup_tree
        .iter()
        .filter_map(|entry| entry.length.map(|length| (entry.path.clone(), length)))
        .collect::<BTreeMap<_, _>>();
    for source_path in &classified.source_files {
        if !is_auxiliary_for_root(source_path, &root_ids) {
            continue;
        }
        let length = lengths.get(source_path).copied().ok_or_else(|| {
            SessionMigrationError::UnrepresentableSource {
                reason: format!(
                    "observed auxiliary file lacks length metadata: {}",
                    source_path.display()
                ),
            }
        })?;
        copy_one_file(backup, source_path, length, root, &stage.join(source_path))?;
    }
    Ok(())
}

fn resumable_root_ids(sessions: &[ClassifiedSession]) -> BTreeSet<String> {
    sessions
        .iter()
        .filter_map(|session| {
            session.entry.rel_path.as_deref().map_or_else(
                || Some(session.entry.id.clone()),
                |relative| {
                    Path::new(relative)
                        .components()
                        .next()
                        .and_then(|component| match component {
                            Component::Normal(value) => value.to_str().map(str::to_owned),
                            _ => None,
                        })
                },
            )
        })
        .collect()
}

fn is_auxiliary_for_root(path: &Path, root_ids: &BTreeSet<String>) -> bool {
    let mut components = path.components();
    let Some(Component::Normal(root)) = components.next() else {
        return false;
    };
    let Some(Component::Normal(family)) = components.next() else {
        return false;
    };
    if !root.to_str().is_some_and(|root| root_ids.contains(root)) {
        return false;
    }
    match family.to_str() {
        Some("spool") => matches!(components.next(), Some(Component::Normal(_))),
        Some("artifacts") => match components.next() {
            Some(Component::Normal(name)) if name == "fetched" => {
                matches!(components.next(), Some(Component::Normal(_)))
            }
            Some(Component::Normal(_)) => true,
            _ => false,
        },
        _ => false,
    }
}

fn require_same_totals(
    expected: TimelineTotals,
    actual: TimelineTotals,
    session_id: &str,
) -> Result<(), SessionMigrationError> {
    if expected.event_count == actual.event_count
        && expected.input_tokens == actual.input_tokens
        && expected.output_tokens == actual.output_tokens
        && expected.cache_read_tokens == actual.cache_read_tokens
    {
        return Ok(());
    }
    Err(SessionMigrationError::UnrepresentableSource {
        reason: format!("legacy timeline '{session_id}' changed after classification"),
    })
}

fn write_jsonl<T: serde::Serialize>(
    root: &PrivateRoot,
    path: &Path,
    rows: &[T],
) -> Result<(), SessionMigrationError> {
    let file = root.create_new(path).map_err(|error| {
        SessionMigrationError::mutation("creating strict JSONL file", path, error)
    })?;
    let mut writer = BufWriter::new(file);
    write_row(&mut writer, &StrictFormatHeader::current(), path)?;
    for row in rows {
        write_row(&mut writer, row, path)?;
    }
    finish_file(writer, path)
}

fn write_json<T: serde::Serialize>(
    root: &PrivateRoot,
    path: &Path,
    value: &T,
) -> Result<(), SessionMigrationError> {
    let file = root.create_new(path).map_err(|error| {
        SessionMigrationError::mutation("creating migration manifest", path, error)
    })?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, value)?;
    writer.write_all(b"\n").map_err(|error| {
        SessionMigrationError::mutation("writing migration manifest", path, error)
    })?;
    finish_file(writer, path)
}

fn write_row<T: serde::Serialize>(
    writer: &mut BufWriter<std::fs::File>,
    value: &T,
    path: &Path,
) -> Result<(), SessionMigrationError> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n").map_err(|error| {
        SessionMigrationError::mutation("writing strict JSONL row", path, error)
    })?;
    Ok(())
}

fn finish_file(
    mut writer: BufWriter<std::fs::File>,
    path: &Path,
) -> Result<(), SessionMigrationError> {
    writer
        .flush()
        .map_err(|error| SessionMigrationError::mutation("flushing migration file", path, error))?;
    let file = writer
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)
        .map_err(|error| {
            SessionMigrationError::mutation("finishing migration file", path, error)
        })?;
    file.sync_all().map_err(|error| {
        SessionMigrationError::mutation("synchronizing migration file", path, error)
    })
}
