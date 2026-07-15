//! Exact checked-tree inventory for P1 evidence tooling.

use std::collections::BTreeSet;

use serde::Deserialize;
use serde_json::Value;

use crate::{Digest, EntryKind, OwnedSnapshot, RepositoryPath, digest_bytes};

use super::authoring::RedactionAuthoringError;
use super::gate_document::{GATE_ENTRYPOINT_PATH, GATE_MANIFEST_PATH, GATE_SCHEMA_PATH};
use super::path_policy::{evidence_tool_paths, is_evidence_tool_candidate, validate_machine_id};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ImplementationRow {
    id: String,
    path: RepositoryPath,
    sha256: Digest,
    status: String,
}

pub(super) fn validate(snapshot: &OwnedSnapshot) -> Result<(), RedactionAuthoringError> {
    let expected = evidence_tool_paths()
        .map(RepositoryPath::parse)
        .collect::<Result<BTreeSet<_>, _>>()
        .map_err(RedactionAuthoringError::InvalidCompiledToolPath)?;
    for path in &expected {
        require_regular(snapshot, path)?;
    }
    let actual = snapshot
        .iter()
        .filter(|(path, _)| is_evidence_tool_candidate(path))
        .map(|(path, _)| path.clone())
        .collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(RedactionAuthoringError::EvidenceToolInventoryMismatch);
    }
    validate_manifest_join(snapshot, &expected)?;
    validate_python_dependencies(snapshot, &expected)
}

fn validate_manifest_join(
    snapshot: &OwnedSnapshot,
    compiled: &BTreeSet<RepositoryPath>,
) -> Result<(), RedactionAuthoringError> {
    let manifest_path = RepositoryPath::parse(GATE_MANIFEST_PATH)
        .map_err(RedactionAuthoringError::InvalidCompiledToolPath)?;
    let manifest =
        crate::strict_json::decode_strict_json::<Value>(require_regular(snapshot, &manifest_path)?)
            .map_err(|_| RedactionAuthoringError::EvidenceToolInventoryMismatch)?;
    let rows = manifest
        .get("implementation")
        .and_then(|value| value.get("files"))
        .and_then(Value::as_array)
        .ok_or(RedactionAuthoringError::EvidenceToolInventoryMismatch)?;
    let mut manifest_paths = BTreeSet::new();
    for value in rows {
        let row = serde_json::from_value::<ImplementationRow>(value.clone())
            .map_err(|_| RedactionAuthoringError::EvidenceToolInventoryMismatch)?;
        if validate_machine_id(&row.id).is_err()
            || !matches!(row.status.as_str(), "active" | "pending_fail_closed")
            || !manifest_paths.insert(row.path.clone())
            || (row.path.as_str() != GATE_SCHEMA_PATH && !compiled.contains(&row.path))
            || digest_bytes(require_regular(snapshot, &row.path)?) != row.sha256
        {
            return Err(RedactionAuthoringError::EvidenceToolInventoryMismatch);
        }
    }
    let entrypoint = RepositoryPath::parse(GATE_ENTRYPOINT_PATH)
        .map_err(RedactionAuthoringError::InvalidCompiledToolPath)?;
    if !compiled.contains(&entrypoint)
        || !manifest_paths
            .iter()
            .any(|path| path.as_str() == "scripts/p1_gate.py")
        || !manifest_paths
            .iter()
            .any(|path| path.as_str() == GATE_SCHEMA_PATH)
    {
        return Err(RedactionAuthoringError::EvidenceToolInventoryMismatch);
    }
    Ok(())
}

fn validate_python_dependencies(
    snapshot: &OwnedSnapshot,
    compiled: &BTreeSet<RepositoryPath>,
) -> Result<(), RedactionAuthoringError> {
    for path in compiled
        .iter()
        .filter(|path| path.file_name().ends_with(".py"))
    {
        let text = std::str::from_utf8(require_regular(snapshot, path)?)
            .map_err(|_| RedactionAuthoringError::EvidenceToolInventoryMismatch)?;
        for module in local_imports(text) {
            let sibling = sibling_module_path(path, module)?;
            if sibling.as_ref().is_some_and(|candidate| {
                snapshot.contains_path(candidate) && !compiled.contains(candidate)
            }) || (is_local_module_name(module)
                && sibling
                    .as_ref()
                    .is_none_or(|candidate| !compiled.contains(candidate)))
            {
                return Err(RedactionAuthoringError::EvidenceToolInventoryMismatch);
            }
        }
    }
    Ok(())
}

fn sibling_module_path(
    source: &RepositoryPath,
    module: &str,
) -> Result<Option<RepositoryPath>, RedactionAuthoringError> {
    if module.contains('.') || module == "__future__" {
        return Ok(None);
    }
    let Some(parent) = source.parent() else {
        return Ok(None);
    };
    RepositoryPath::parse(format!("{parent}/{module}.py"))
        .map(Some)
        .map_err(|_| RedactionAuthoringError::EvidenceToolInventoryMismatch)
}

fn local_imports(source: &str) -> Vec<&str> {
    let mut modules = Vec::new();
    for line in source.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("from ") {
            if let Some(module) = rest.split_ascii_whitespace().next() {
                modules.push(module);
            }
        } else if let Some(rest) = line.strip_prefix("import ") {
            modules.extend(
                rest.split(',')
                    .filter_map(|member| member.trim_start().split_ascii_whitespace().next()),
            );
        }
    }
    modules
}

fn is_local_module_name(module: &str) -> bool {
    ["p1_", "test_p1_", "openai_contract_", "responses_fixture_"]
        .iter()
        .any(|prefix| module.starts_with(prefix))
}

fn require_regular<'a>(
    snapshot: &'a OwnedSnapshot,
    path: &RepositoryPath,
) -> Result<&'a [u8], RedactionAuthoringError> {
    let Some(entry) = snapshot.get(path) else {
        return Err(RedactionAuthoringError::MissingEvidenceToolSource);
    };
    if entry.kind() != EntryKind::Regular {
        return Err(RedactionAuthoringError::NonRegularCheckedTreeArtifact);
    }
    Ok(entry.bytes())
}
