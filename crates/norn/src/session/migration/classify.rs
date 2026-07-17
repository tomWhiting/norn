use std::collections::{BTreeMap, BTreeSet};
use std::io::BufReader;
use std::path::{Component, Path, PathBuf};

use crate::session::persistence::io::ensure_session_id_path_safe;
use crate::session::persistence::strict::ResumeFidelity;
use crate::util::{PrivateEntryKind, PrivateRootReader, PrivateTreeEntry};

use super::error::SessionMigrationError;
use super::json::{decode_known_value, parse_unique_json};
use super::legacy_index::{LegacySessionIndexEntry, timeline_path};
use super::types::{LegacyClassificationReason, LegacySessionMigrationRecord};

#[path = "classify_lines.rs"]
mod raw_lines;
#[path = "classify_path.rs"]
mod source_path;
#[path = "classify_timeline.rs"]
mod timeline;
use raw_lines::PhysicalLines;
pub(super) use source_path::{decode_relative_path, encode_relative_path};
use timeline::scan_timeline;
pub(super) use timeline::{hash_file, visit_legacy_events};

const INDEX_FILE: &str = "index.jsonl";

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct TimelineTotals {
    pub(super) event_count: u64,
    pub(super) input_tokens: u64,
    pub(super) output_tokens: u64,
    pub(super) cache_read_tokens: u64,
}

#[derive(Clone, Debug)]
pub(super) struct ClassifiedSession {
    pub(super) entry: LegacySessionIndexEntry,
    pub(super) source_path: PathBuf,
    pub(super) source_sha256: String,
    pub(super) source_format: u32,
    pub(super) fidelity: ResumeFidelity,
    pub(super) totals: TimelineTotals,
}

#[derive(Debug)]
pub(super) struct ClassifiedLegacyStore {
    pub(super) sessions: Vec<ClassifiedSession>,
    pub(super) records: Vec<LegacySessionMigrationRecord>,
    pub(super) source_files: BTreeSet<PathBuf>,
}

#[derive(Debug)]
struct IndexCandidate {
    line: u64,
    entry: LegacySessionIndexEntry,
    source_path: PathBuf,
}

pub(super) fn classify_legacy_store(
    reader: &PrivateRootReader,
    tree: &[PrivateTreeEntry],
) -> Result<ClassifiedLegacyStore, SessionMigrationError> {
    let source_files = tree
        .iter()
        .filter(|entry| entry.kind == PrivateEntryKind::File)
        .map(|entry| entry.path.clone())
        .collect::<BTreeSet<_>>();
    let (candidates, mut records) = read_index(reader, &source_files)?;
    let (duplicate_ids, duplicate_paths) = duplicate_claims(&candidates);
    let claimed_paths = candidates
        .iter()
        .map(|candidate| candidate.source_path.clone())
        .collect::<BTreeSet<_>>();
    let mut sessions = Vec::new();

    for candidate in candidates {
        let mut conflict_reasons = Vec::new();
        if duplicate_ids.contains(&candidate.entry.id) {
            conflict_reasons.push(LegacyClassificationReason::DuplicateSessionId);
        }
        if duplicate_paths.contains(&candidate.source_path) {
            conflict_reasons.push(LegacyClassificationReason::DuplicateTimelinePath);
        }
        if !conflict_reasons.is_empty() {
            let (source_path, source_sha256) = if source_files.contains(&candidate.source_path) {
                (
                    candidate.source_path.clone(),
                    hash_file(reader, &candidate.source_path)?,
                )
            } else {
                (
                    PathBuf::from(INDEX_FILE),
                    hash_file(reader, Path::new(INDEX_FILE))?,
                )
            };
            let mut record = record_for_candidate(
                &candidate,
                Some(source_sha256),
                Some(candidate.entry.format_version),
                ResumeFidelity::InspectOnly,
                conflict_reasons,
                None,
            )?;
            record.source_path = Some(encode_relative_path(&source_path)?);
            records.push(record);
            continue;
        }

        if !source_files.contains(&candidate.source_path) {
            let index_sha256 = hash_file(reader, Path::new(INDEX_FILE))?;
            records.push(LegacySessionMigrationRecord {
                catalog_id: None,
                session_id: Some(candidate.entry.id.clone()),
                source_index_line: Some(candidate.line),
                source_path: Some(INDEX_FILE.to_owned()),
                source_sha256: Some(index_sha256),
                source_format: Some(candidate.entry.format_version),
                fidelity: ResumeFidelity::InspectOnly,
                reasons: vec![LegacyClassificationReason::MissingTimeline],
                destination_path: None,
            });
            continue;
        }

        let mut scan = scan_timeline(
            reader,
            &candidate.source_path,
            candidate.entry.format_version,
        )?;
        if scan.fidelity != ResumeFidelity::InspectOnly
            && (candidate.entry.event_count != scan.totals.event_count
                || candidate.entry.total_input_tokens != scan.totals.input_tokens
                || candidate.entry.total_output_tokens != scan.totals.output_tokens
                || candidate.entry.total_cache_read_tokens != scan.totals.cache_read_tokens)
        {
            scan.reasons
                .push(LegacyClassificationReason::StaleIndexMetadata);
        }
        let Some(source_format) = scan.source_format else {
            records.push(record_for_candidate(
                &candidate,
                Some(scan.sha256),
                None,
                ResumeFidelity::InspectOnly,
                scan.reasons,
                None,
            )?);
            continue;
        };
        let destination = candidate.source_path.clone();
        let destination =
            (scan.fidelity != ResumeFidelity::InspectOnly).then_some(destination.as_path());
        records.push(record_for_candidate(
            &candidate,
            Some(scan.sha256.clone()),
            Some(source_format),
            scan.fidelity,
            scan.reasons.clone(),
            destination,
        )?);
        if scan.fidelity != ResumeFidelity::InspectOnly {
            sessions.push(ClassifiedSession {
                entry: candidate.entry,
                source_path: candidate.source_path,
                source_sha256: scan.sha256,
                source_format,
                fidelity: scan.fidelity,
                totals: scan.totals,
            });
        }
    }

    for path in source_files.iter().filter(|path| {
        path.as_path() != Path::new(INDEX_FILE)
            && looks_like_timeline(path)
            && !claimed_paths.contains(*path)
    }) {
        let digest = hash_file(reader, path)?;
        records.push(LegacySessionMigrationRecord {
            catalog_id: None,
            session_id: None,
            source_index_line: None,
            source_path: Some(encode_relative_path(path)?),
            source_sha256: Some(digest),
            source_format: None,
            fidelity: ResumeFidelity::InspectOnly,
            reasons: vec![LegacyClassificationReason::OrphanTimeline],
            destination_path: None,
        });
    }

    super::classify_relationships::demote_invalid_relationships(&mut sessions, &mut records)?;
    Ok(ClassifiedLegacyStore {
        sessions,
        records,
        source_files,
    })
}

fn read_index(
    reader: &PrivateRootReader,
    source_files: &BTreeSet<PathBuf>,
) -> Result<(Vec<IndexCandidate>, Vec<LegacySessionMigrationRecord>), SessionMigrationError> {
    if !source_files.contains(Path::new(INDEX_FILE)) {
        return Ok((Vec::new(), Vec::new()));
    }
    let index_sha256 = hash_file(reader, Path::new(INDEX_FILE))?;
    let file = reader
        .open_file(Path::new(INDEX_FILE))
        .map_err(|error| SessionMigrationError::observation(INDEX_FILE, error))?;
    let mut lines = PhysicalLines::new(BufReader::new(file));
    let mut candidates = Vec::new();
    let mut records = Vec::new();
    while let Some(line) = lines
        .next_line()
        .map_err(|error| SessionMigrationError::observation(PathBuf::from(INDEX_FILE), error))?
    {
        if line.bytes.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if !line.terminated {
            records.push(invalid_index_record(
                line.number,
                "index row is not newline-terminated".to_owned(),
                &index_sha256,
            ));
            continue;
        }
        let value = match parse_unique_json(&line.bytes) {
            Ok(value) => value,
            Err(diagnostic) => {
                records.push(invalid_index_record(line.number, diagnostic, &index_sha256));
                continue;
            }
        };
        let decoded =
            decode_known_value::<LegacySessionIndexEntry>(value).and_then(validate_index_entry);
        match decoded {
            Ok(entry) => {
                let source_path = timeline_path(&entry)
                    .map_err(|reason| SessionMigrationError::UnrepresentableSource { reason })?;
                candidates.push(IndexCandidate {
                    line: line.number,
                    entry,
                    source_path,
                });
            }
            Err(diagnostic) => {
                records.push(invalid_index_record(line.number, diagnostic, &index_sha256));
            }
        }
    }
    Ok((candidates, records))
}

fn validate_index_entry(entry: LegacySessionIndexEntry) -> Result<LegacySessionIndexEntry, String> {
    ensure_session_id_path_safe(&entry.id).map_err(|error| error.to_string())?;
    timeline_path(&entry)?;
    if let Some(parent_id) = entry.parent_id.as_deref() {
        ensure_session_id_path_safe(parent_id).map_err(|error| error.to_string())?;
    }
    if entry.format_version > 1 {
        return Err(format!(
            "index claims unsupported legacy format {}",
            entry.format_version
        ));
    }
    if !is_normalized_absolute(Path::new(&entry.working_dir)) {
        return Err("working_dir must be an absolute normalized path".to_owned());
    }
    Ok(entry)
}

fn is_normalized_absolute(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir | Component::Normal(_) => normalized.push(component.as_os_str()),
            Component::CurDir | Component::ParentDir => return false,
        }
    }
    normalized == path
}

fn duplicate_claims(candidates: &[IndexCandidate]) -> (BTreeSet<String>, BTreeSet<PathBuf>) {
    let mut ids = BTreeMap::<&str, usize>::new();
    let mut paths = BTreeMap::<&Path, usize>::new();
    for candidate in candidates {
        *ids.entry(&candidate.entry.id).or_default() += 1;
        *paths.entry(&candidate.source_path).or_default() += 1;
    }
    let duplicate_ids = ids
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(id, _)| id.to_owned())
        .collect();
    let duplicate_paths = paths
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(path, _)| path.to_path_buf())
        .collect();
    (duplicate_ids, duplicate_paths)
}

fn record_for_candidate(
    candidate: &IndexCandidate,
    source_sha256: Option<String>,
    source_format: Option<u32>,
    fidelity: ResumeFidelity,
    reasons: Vec<LegacyClassificationReason>,
    destination: Option<&Path>,
) -> Result<LegacySessionMigrationRecord, SessionMigrationError> {
    Ok(LegacySessionMigrationRecord {
        catalog_id: None,
        session_id: Some(candidate.entry.id.clone()),
        source_index_line: Some(candidate.line),
        source_path: Some(encode_relative_path(&candidate.source_path)?),
        source_sha256,
        source_format,
        fidelity,
        reasons,
        destination_path: destination.map(encode_relative_path).transpose()?,
    })
}

fn invalid_index_record(
    line: u64,
    diagnostic: String,
    index_sha256: &str,
) -> LegacySessionMigrationRecord {
    LegacySessionMigrationRecord {
        catalog_id: None,
        session_id: None,
        source_index_line: Some(line),
        source_path: Some(INDEX_FILE.to_owned()),
        source_sha256: Some(index_sha256.to_owned()),
        source_format: None,
        fidelity: ResumeFidelity::InspectOnly,
        reasons: vec![LegacyClassificationReason::InvalidIndexRow {
            line: Some(line),
            diagnostic,
        }],
        destination_path: None,
    }
}

fn looks_like_timeline(path: &Path) -> bool {
    let components = path.components().collect::<Vec<_>>();
    match components.as_slice() {
        [Component::Normal(file)] => {
            file != &std::ffi::OsStr::new(INDEX_FILE)
                && Path::new(file)
                    .extension()
                    .is_some_and(|ext| ext == "jsonl")
        }
        [
            Component::Normal(_),
            Component::Normal(children),
            Component::Normal(file),
        ] => {
            children == &std::ffi::OsStr::new("children")
                && Path::new(file)
                    .extension()
                    .is_some_and(|ext| ext == "jsonl")
        }
        _ => false,
    }
}
