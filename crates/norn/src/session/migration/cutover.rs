use std::io::{BufReader, Read as _, Write as _};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::session::persistence::strict::read_strict_index_file;
use crate::util::{PrivateRoot, PrivateRootReader};

use super::error::SessionMigrationError;
use super::json::{decode_known_value, parse_unique_json};
use super::stage_ownership::{STAGE_OWNERSHIP_VERSION, StageKind, verify_reader_marker};
use super::types::{MIGRATION_MANIFEST_FILE, STRICT_SESSION_DIRECTORY};

pub(super) const CUTOVER_RECEIPT_FILE: &str = "migration-cutover-receipt.json";
pub(super) const CUTOVER_RECEIPT_VERSION: u32 = 1;
const INDEX_FILE: &str = "index.jsonl";
/// Immutable publication-time evidence; never a live runtime index authority.
pub(super) const INITIAL_INDEX_FILE: &str = "migration-initial-index.jsonl";
const RECEIPT_TEMPLATE: &str = concat!(
    "{\"receipt_version\":1,\"stage_ownership_version\":1,",
    "\"source_tree_sha256\":\"0000000000000000000000000000000000000000000000000000000000000000\",",
    "\"manifest_sha256\":\"0000000000000000000000000000000000000000000000000000000000000000\",",
    "\"initial_index_sha256\":\"0000000000000000000000000000000000000000000000000000000000000000\"}\n",
);
const RECEIPT_BYTES: u64 = RECEIPT_TEMPLATE.len() as u64;

#[derive(Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CutoverReceipt {
    receipt_version: u32,
    stage_ownership_version: u32,
    source_tree_sha256: String,
    manifest_sha256: String,
    initial_index_sha256: String,
}

/// Write the final bounded proof only after the manifest and index are durable.
pub(super) fn write_cutover_receipt(
    root: &PrivateRoot,
    stage: &Path,
    source_tree_sha256: &str,
) -> Result<(), SessionMigrationError> {
    write_initial_index_snapshot(root, stage)?;
    let receipt = CutoverReceipt {
        receipt_version: CUTOVER_RECEIPT_VERSION,
        stage_ownership_version: STAGE_OWNERSHIP_VERSION,
        source_tree_sha256: source_tree_sha256.to_owned(),
        manifest_sha256: hash_private_file(root, &stage.join(MIGRATION_MANIFEST_FILE))?,
        initial_index_sha256: hash_private_file(root, &stage.join(INITIAL_INDEX_FILE))?,
    };
    let relative = stage.join(CUTOVER_RECEIPT_FILE);
    let file = root.create_new(&relative).map_err(|error| {
        SessionMigrationError::mutation("creating migration cutover receipt", &relative, error)
    })?;
    let mut writer = std::io::BufWriter::new(file);
    serde_json::to_writer(&mut writer, &receipt)?;
    writer.write_all(b"\n").map_err(|error| {
        SessionMigrationError::mutation("writing migration cutover receipt", &relative, error)
    })?;
    writer.flush().map_err(|error| {
        SessionMigrationError::mutation("flushing migration cutover receipt", &relative, error)
    })?;
    let file = writer
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)
        .map_err(|error| {
            SessionMigrationError::mutation("finishing migration cutover receipt", &relative, error)
        })?;
    file.sync_all().map_err(|error| {
        SessionMigrationError::mutation("synchronizing migration cutover receipt", &relative, error)
    })?;
    root.sync_dir(stage).map_err(|error| {
        SessionMigrationError::mutation("publishing migration cutover receipt", stage, error)
    })
}

/// Verify only bounded metadata inside the atomically published strict store.
pub(super) fn verify_published_cutover(norn_root: &Path) -> Result<(), SessionMigrationError> {
    let store_path = norn_root.join(STRICT_SESSION_DIRECTORY);
    let store = PrivateRootReader::open(&store_path)
        .map_err(|error| SessionMigrationError::observation(&store_path, error))?;
    verify_cutover_reader(&store, &store_path)
}

pub(super) fn verify_cutover_reader(
    store: &PrivateRootReader,
    store_path: &Path,
) -> Result<(), SessionMigrationError> {
    let receipt = read_receipt(store, store_path)?;
    verify_reader_marker(
        store,
        store_path,
        StageKind::StrictStore,
        Some(&receipt.source_tree_sha256),
    )?;
    for required in [MIGRATION_MANIFEST_FILE, INITIAL_INDEX_FILE, INDEX_FILE] {
        let path = store_path.join(required);
        let file = store
            .open_file(Path::new(required))
            .map_err(|error| SessionMigrationError::observation(path, error))?;
        drop(file);
    }
    Ok(())
}

/// Recompute publication-time receipt hashes for explicit offline verification.
pub(super) fn verify_cutover_artifacts(
    store: &PrivateRootReader,
    store_path: &Path,
) -> Result<(), SessionMigrationError> {
    let receipt = read_receipt(store, store_path)?;
    verify_reader_marker(
        store,
        store_path,
        StageKind::StrictStore,
        Some(&receipt.source_tree_sha256),
    )?;
    verify_file_digest(
        store,
        store_path,
        MIGRATION_MANIFEST_FILE,
        &receipt.manifest_sha256,
    )?;
    verify_file_digest(
        store,
        store_path,
        INITIAL_INDEX_FILE,
        &receipt.initial_index_sha256,
    )?;
    let initial_index_path = store_path.join(INITIAL_INDEX_FILE);
    let initial_index = store
        .open_file(Path::new(INITIAL_INDEX_FILE))
        .map_err(|error| SessionMigrationError::observation(&initial_index_path, error))?;
    let _validated_initial_index =
        read_strict_index_file(BufReader::new(initial_index), &initial_index_path)?;
    let index_path = store_path.join(INDEX_FILE);
    let index = store
        .open_file(Path::new(INDEX_FILE))
        .map_err(|error| SessionMigrationError::observation(&index_path, error))?;
    drop(index);
    Ok(())
}

fn read_receipt(
    store: &PrivateRootReader,
    store_path: &Path,
) -> Result<CutoverReceipt, SessionMigrationError> {
    let receipt_path = store_path.join(CUTOVER_RECEIPT_FILE);
    let receipt_file = store
        .open_file(Path::new(CUTOVER_RECEIPT_FILE))
        .map_err(|error| SessionMigrationError::observation(&receipt_path, error))?;
    let mut bytes = Vec::new();
    receipt_file
        .take(RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| SessionMigrationError::observation(&receipt_path, error))?;
    if bytes.len() != RECEIPT_TEMPLATE.len() {
        return Err(SessionMigrationError::UnrepresentableSource {
            reason: "migration cutover receipt has the wrong encoded length".to_owned(),
        });
    }
    let value = parse_unique_json(&bytes)
        .map_err(|reason| SessionMigrationError::UnrepresentableSource { reason })?;
    let receipt: CutoverReceipt = decode_known_value(value)
        .map_err(|reason| SessionMigrationError::UnrepresentableSource { reason })?;
    let mut canonical = serde_json::to_vec(&receipt)?;
    canonical.push(b'\n');
    if canonical != bytes {
        return Err(SessionMigrationError::UnrepresentableSource {
            reason: "migration cutover receipt is not in canonical encoding".to_owned(),
        });
    }
    validate_receipt(&receipt)?;
    Ok(receipt)
}

fn validate_receipt(receipt: &CutoverReceipt) -> Result<(), SessionMigrationError> {
    if receipt.receipt_version != CUTOVER_RECEIPT_VERSION
        || receipt.stage_ownership_version != STAGE_OWNERSHIP_VERSION
        || !is_digest(&receipt.source_tree_sha256)
        || !is_digest(&receipt.manifest_sha256)
        || !is_digest(&receipt.initial_index_sha256)
    {
        return Err(SessionMigrationError::UnrepresentableSource {
            reason: "migration cutover receipt has an unsupported version or invalid digest"
                .to_owned(),
        });
    }
    Ok(())
}

fn write_initial_index_snapshot(
    root: &PrivateRoot,
    stage: &Path,
) -> Result<(), SessionMigrationError> {
    let source_path = stage.join(INDEX_FILE);
    let mut source = root.open_read(&source_path).map_err(|error| {
        SessionMigrationError::mutation("reading initial strict index", &source_path, error)
    })?;
    let snapshot_path = stage.join(INITIAL_INDEX_FILE);
    let snapshot = root.create_new(&snapshot_path).map_err(|error| {
        SessionMigrationError::mutation("creating initial-index receipt", &snapshot_path, error)
    })?;
    let mut snapshot = std::io::BufWriter::new(snapshot);
    std::io::copy(&mut source, &mut snapshot).map_err(|error| {
        SessionMigrationError::mutation("copying initial-index receipt", &snapshot_path, error)
    })?;
    snapshot.flush().map_err(|error| {
        SessionMigrationError::mutation("flushing initial-index receipt", &snapshot_path, error)
    })?;
    let snapshot = snapshot
        .into_inner()
        .map_err(std::io::IntoInnerError::into_error)
        .map_err(|error| {
            SessionMigrationError::mutation(
                "finishing initial-index receipt",
                &snapshot_path,
                error,
            )
        })?;
    snapshot.sync_all().map_err(|error| {
        SessionMigrationError::mutation(
            "synchronizing initial-index receipt",
            &snapshot_path,
            error,
        )
    })?;
    root.sync_dir(stage).map_err(|error| {
        SessionMigrationError::mutation("publishing initial-index receipt", stage, error)
    })
}

fn verify_file_digest(
    reader: &PrivateRootReader,
    root: &Path,
    relative: &str,
    expected: &str,
) -> Result<(), SessionMigrationError> {
    let path = Path::new(relative);
    let display = root.join(path);
    let file = reader
        .open_file(path)
        .map_err(|error| SessionMigrationError::observation(&display, error))?;
    let actual = hash_reader(BufReader::new(file), &display)?;
    if actual != expected {
        return Err(SessionMigrationError::CutoverReceiptConflict {
            path: display,
            expected_sha256: expected.to_owned(),
            actual_sha256: actual,
        });
    }
    Ok(())
}

fn hash_private_file(root: &PrivateRoot, path: &Path) -> Result<String, SessionMigrationError> {
    let file = root.open_read(path).map_err(|error| {
        SessionMigrationError::mutation("reading migration receipt input", path, error)
    })?;
    hash_reader(BufReader::new(file), path)
}

fn hash_reader<R: std::io::BufRead>(
    mut reader: R,
    path: &Path,
) -> Result<String, SessionMigrationError> {
    let mut hasher = Sha256::new();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| SessionMigrationError::observation(path, error))?;
        if available.is_empty() {
            break;
        }
        hasher.update(available);
        let consumed = available.len();
        reader.consume(consumed);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn is_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}
