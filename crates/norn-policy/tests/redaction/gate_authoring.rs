use std::error::Error;
use std::io;
use std::path::Path;

use norn_policy::redaction::{
    ArtifactFamily, ObservationSource, RedactionAuthoringError, RedactionRegistry,
    p1_evidence_tool_paths,
};
use norn_policy::{OwnedSnapshot, RepositoryPath, SnapshotEntry, digest_bytes};
use serde_json::{Value, json};

use super::support::{protocol_bytes, replace};

const CANDIDATE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TREE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const RUN: &str = "run-fixture";
const TIMESTAMP: &str = "2026-07-15T00:00:00.000000Z";
const TRACEABILITY_BYTES: &[u8] =
    include_bytes!("../../../../docs/reviews/evidence/p1/finding-traceability.jsonl");
const CONTRACT_BYTES: &[u8] =
    include_bytes!("../../../../policy/contracts/openai-responses-v1/manifest.json");

#[test]
fn run_local_authoring_binds_real_layout_and_checked_authority() -> Result<(), Box<dyn Error>> {
    let (checked_registry, checked_snapshot) = checked_authority()?;
    let run_snapshot = gate_run(&checked_snapshot)?;
    let first = RedactionRegistry::author_run_local_p1(
        &checked_registry,
        &checked_snapshot,
        &run_snapshot,
    )?;
    let second = RedactionRegistry::author_run_local_p1(
        &checked_registry,
        &checked_snapshot,
        &run_snapshot,
    )?;
    assert_eq!(first, second);
    assert_eq!(first.registered_paths().len(), 30);

    let log_path = RepositoryPath::parse(format!(
        "target/p1-gate/evidence/{CANDIDATE}/{RUN}/01-rustc-version.stdout.log"
    ))?;
    let changed = replace(
        &run_snapshot,
        &log_path,
        SnapshotEntry::regular(b"result=fail tests=0\n".to_vec()),
    )?;
    let changed_result =
        RedactionRegistry::author_run_local_p1(&checked_registry, &checked_snapshot, &changed);
    assert!(
        matches!(
            changed_result,
            Err(RedactionAuthoringError::InvalidRunLocalObservation)
        ),
        "unexpected changed-log result: {changed_result:?}"
    );
    Ok(())
}

#[test]
fn run_local_authoring_rejects_unpinned_checked_authority() -> Result<(), Box<dyn Error>> {
    let (checked_registry, checked_snapshot) = checked_authority()?;
    let run_snapshot = gate_run(&checked_snapshot)?;
    let manifest_path = RepositoryPath::parse("policy/gate-commands.json")?;
    let changed = replace(
        &checked_snapshot,
        &manifest_path,
        SnapshotEntry::regular(b"{}\n".to_vec()),
    )?;
    assert!(matches!(
        RedactionRegistry::author_run_local_p1(&checked_registry, &changed, &run_snapshot),
        Err(RedactionAuthoringError::CheckedAuthorityValidation)
    ));
    Ok(())
}

#[test]
fn checked_authoring_uses_the_same_rules_for_promoted_gate_evidence() -> Result<(), Box<dyn Error>>
{
    let (checked_registry, checked_snapshot) = checked_authority()?;
    let run_snapshot = gate_run(&checked_snapshot)?;
    let packaged = promote_gate_run(&run_snapshot)?;
    let combined = OwnedSnapshot::try_from_entries(
        checked_snapshot
            .iter()
            .chain(packaged.iter())
            .map(|(path, entry)| (path.clone(), entry.clone())),
    )?;
    let registry = RedactionRegistry::author_checked_tree_p1(&combined)?;
    assert_eq!(
        registry.registered_paths().len(),
        checked_registry.registered_paths().len() + 30
    );
    Ok(())
}

fn checked_authority() -> Result<(RedactionRegistry, OwnedSnapshot), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut entries = p1_evidence_tool_paths()
        .chain([
            "policy/gate-commands.json",
            "policy/evidence-schemas/gate-run.schema.json",
        ])
        .map(|raw_path| {
            Ok((
                RepositoryPath::parse(raw_path)?,
                SnapshotEntry::regular(std::fs::read(root.join(raw_path))?),
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    entries.extend([
        (
            RepositoryPath::parse(
                "crates/norn/testdata/openai_responses/public/requests/request.json",
            )?,
            SnapshotEntry::regular(protocol_bytes()?),
        ),
        (
            RepositoryPath::parse("docs/reviews/evidence/p1/finding-traceability.jsonl")?,
            SnapshotEntry::regular(TRACEABILITY_BYTES.to_vec()),
        ),
        (
            RepositoryPath::parse("policy/contracts/openai-responses-v1/manifest.json")?,
            SnapshotEntry::regular(CONTRACT_BYTES.to_vec()),
        ),
    ]);
    let snapshot = OwnedSnapshot::try_from_entries(entries)?;
    let registry = RedactionRegistry::author_checked_tree_p1(&snapshot)?;
    Ok((registry, snapshot))
}

fn gate_run(checked: &OwnedSnapshot) -> Result<OwnedSnapshot, Box<dyn Error>> {
    let manifest_bytes = checked_bytes(checked, "policy/gate-commands.json")?;
    let manifest = serde_json::from_slice::<Value>(manifest_bytes)?;
    let manifest_commands = manifest
        .get("commands")
        .and_then(Value::as_array)
        .ok_or_else(|| io::Error::other("gate command inventory missing"))?;
    let pinned_files = manifest
        .get("implementation")
        .and_then(|value| value.get("files"))
        .cloned()
        .ok_or_else(|| io::Error::other("gate implementation inventory missing"))?;
    let resource_limits = manifest
        .get("resource_limits")
        .cloned()
        .ok_or_else(|| io::Error::other("gate resource limits missing"))?;
    let prefix = format!("target/p1-gate/evidence/{CANDIDATE}/{RUN}");
    let mut entries = Vec::new();
    let mut records = Vec::new();
    for (offset, command) in manifest_commands.iter().enumerate() {
        let order = offset
            .checked_add(1)
            .ok_or_else(|| io::Error::other("gate command order overflow"))?;
        let id = command
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| io::Error::other("gate command id missing"))?;
        let kind = command
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| io::Error::other("gate command kind missing"))?;
        let test_executions = if kind == "distribution" { 20 } else { 0 };
        let stdout = if kind == "distribution" {
            b"result=pass tests=20 passed=20 failed=0\n".to_vec()
        } else {
            b"result=pass tests=0\n".to_vec()
        };
        let stderr = b"result=pass exit_status=0\n".to_vec();
        let stdout_name = format!("{order:02}-{id}.stdout.log");
        let stderr_name = format!("{order:02}-{id}.stderr.log");
        let stdout_path = RepositoryPath::parse(format!("{prefix}/{stdout_name}"))?;
        let stderr_path = RepositoryPath::parse(format!("{prefix}/{stderr_name}"))?;
        let stdout_digest = digest_bytes(&stdout);
        let stderr_digest = digest_bytes(&stderr);
        entries.extend([
            (stdout_path.clone(), SnapshotEntry::regular(stdout.clone())),
            (stderr_path, SnapshotEntry::regular(stderr.clone())),
        ]);
        let distribution = if kind == "distribution" {
            let sidecar_path =
                RepositoryPath::parse(format!("{prefix}/{order:02}-{id}.distribution.json"))?;
            let sidecar = serde_json::to_vec(&json!({
                "artifact_family": ArtifactFamily::Distribution,
                "artifact_id": format!("p1-distribution-{id}"),
                "observations": [{
                    "digest": stdout_digest,
                    "id": format!("distribution-{id}-stdout"),
                    "referenced_family": ArtifactFamily::SanitizedLog,
                    "referenced_path": stdout_path,
                    "source": ObservationSource::LocalGate,
                    "synthetic_ids": []
                }],
                "schema_version": 1,
                "synthetic_values": []
            }))?;
            entries.push((sidecar_path, SnapshotEntry::regular(sidecar)));
            json!({"observations": 20, "passed": 20, "failed": 0})
        } else {
            Value::Null
        };
        records.push(json!({
            "argv": command.get("argv").cloned().ok_or_else(|| io::Error::other("argv missing"))?,
            "completed_at": TIMESTAMP,
            "distribution": distribution,
            "exit_code": 0,
            "failure_code": null,
            "id": id,
            "kind": kind,
            "order": order,
            "outcome": "passed",
            "process_outcome": "passed",
            "started_at": TIMESTAMP,
            "stderr": {"bytes": stderr.len(), "path": stderr_name, "sha256": stderr_digest},
            "stdout": {"bytes": stdout.len(), "path": stdout_name, "sha256": stdout_digest},
            "test_executions": test_executions,
            "tool": {"id": "fixture-tool", "sha256": digest_bytes(b"tool")}
        }));
    }
    let repository = repository_snapshot();
    let descriptor = json!({
        "base": {"commit": CANDIDATE, "tree": TREE},
        "candidate": {"commit": CANDIDATE, "tree": TREE},
        "commands": records,
        "completed_at": TIMESTAMP,
        "environment": fixture_environment(),
        "evidence_id": "p1-gate-local-001",
        "failure_codes": [],
        "gate": {
            "command_manifest_path": "policy/gate-commands.json",
            "command_manifest_sha256": digest_bytes(manifest_bytes),
            "entrypoint_path": "scripts/p1-gate",
            "entrypoint_sha256": digest_bytes(checked_bytes(checked, "scripts/p1-gate")?),
            "pinned_files": pinned_files
        },
        "interpreter": {"id": "python", "sha256": digest_bytes(b"python"), "version": "3.14.0"},
        "outcome": "passed",
        "phase": "P1",
        "repository_end": repository,
        "repository_start": repository,
        "resource_limits": resource_limits,
        "schema_version": 1,
        "started_at": TIMESTAMP,
        "tools": [{"id": "fixture-tool", "sha256": digest_bytes(b"tool")}]
    });
    entries.push((
        RepositoryPath::parse(format!("{prefix}/descriptor.json"))?,
        SnapshotEntry::regular(serde_json::to_vec(&descriptor)?),
    ));
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}

fn promote_gate_run(run: &OwnedSnapshot) -> Result<OwnedSnapshot, Box<dyn Error>> {
    let target_prefix = format!("target/p1-gate/evidence/{CANDIDATE}/{RUN}");
    let log_prefix = format!("docs/reviews/evidence/p1/gate/logs/{CANDIDATE}/{RUN}");
    let descriptor_prefix = format!("docs/reviews/evidence/p1/gate/descriptors/{CANDIDATE}/{RUN}");
    let distribution_prefix =
        format!("docs/reviews/evidence/p1/gate/distributions/{CANDIDATE}/{RUN}");
    let mut entries = Vec::new();
    for (path, entry) in run.iter() {
        let Some(name) = path.as_str().strip_prefix(&format!("{target_prefix}/")) else {
            return Err(io::Error::other("target gate path escaped its run root").into());
        };
        let (promoted_path, promoted_entry) = if name == "descriptor.json" {
            (
                RepositoryPath::parse(format!("{descriptor_prefix}/{name}"))?,
                entry.clone(),
            )
        } else if name.ends_with(".distribution.json") {
            let mut sidecar = serde_json::from_slice::<Value>(entry.bytes())?;
            let observation = sidecar
                .get_mut("observations")
                .and_then(Value::as_array_mut)
                .and_then(|rows| rows.first_mut())
                .and_then(Value::as_object_mut)
                .ok_or_else(|| io::Error::other("distribution observation missing"))?;
            let stdout_name = name.replace(".distribution.json", ".stdout.log");
            observation.insert(
                "referenced_path".to_owned(),
                Value::String(format!("{log_prefix}/{stdout_name}")),
            );
            (
                RepositoryPath::parse(format!("{distribution_prefix}/{name}"))?,
                SnapshotEntry::regular(serde_json::to_vec(&sidecar)?),
            )
        } else {
            (
                RepositoryPath::parse(format!("{log_prefix}/{name}"))?,
                entry.clone(),
            )
        };
        entries.push((promoted_path, promoted_entry));
    }
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}

fn checked_bytes<'a>(
    snapshot: &'a OwnedSnapshot,
    raw_path: &str,
) -> Result<&'a [u8], Box<dyn Error>> {
    let path = RepositoryPath::parse(raw_path)?;
    snapshot
        .get(&path)
        .map(SnapshotEntry::bytes)
        .ok_or_else(|| io::Error::other("checked gate authority missing").into())
}

fn repository_snapshot() -> Value {
    json!({
        "clean": true,
        "commit": CANDIDATE,
        "conflict_sha256": digest_bytes(b"conflict"),
        "status_sha256": digest_bytes(b"status"),
        "submodule_sha256": digest_bytes(b"submodule"),
        "submodules_clean": true,
        "tree": TREE
    })
}

fn fixture_environment() -> Value {
    json!({
        "cache_bridges": [],
        "caller_environment_inherited": [],
        "controlled": {
            "CARGO_BUILD_JOBS": "1",
            "CARGO_HOME": "target/p1-gate/home",
            "CARGO_INCREMENTAL": "0",
            "CARGO_NET_OFFLINE": "true",
            "CARGO_TARGET_DIR": "target/p1-gate/cargo-target",
            "CARGO_TERM_COLOR": "never",
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_CONFIG_SYSTEM": "/dev/null",
            "GIT_PAGER": "cat",
            "GIT_TERMINAL_PROMPT": "0",
            "HOME": "target/p1-gate/home",
            "LANG": "C",
            "LC_ALL": "C",
            "NO_COLOR": "1",
            "PAGER": "cat",
            "PATH": ["selected-rust-toolchain", "system-usr-bin", "system-bin"],
            "PYTHONDONTWRITEBYTECODE": "1",
            "RUST_BACKTRACE": "0",
            "RUSTC": "tool:rustc",
            "RUSTDOC": "tool:rustdoc",
            "SDKROOT": null,
            "TERM": "dumb",
            "TMPDIR": "target/p1-gate/tmp",
            "TZ": "UTC"
        },
        "credential_environment_inherited": []
    })
}
