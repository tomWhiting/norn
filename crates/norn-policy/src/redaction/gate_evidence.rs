//! Shared derivation for target-local and promoted P1 gate evidence.

use std::collections::{BTreeMap, BTreeSet};

use crate::{EntryKind, OwnedSnapshot, RepositoryPath, digest_bytes};

use super::RedactionRegistry;
use super::authoring::{RedactionAuthoringError, artifact_id};
use super::evidence_document::EvidenceDocument;
use super::gate_document::{GATE_DESCRIPTOR_NAME, GateCommand, decode_gate_run};
use super::model::{
    ArtifactFamily, ArtifactRegistration, ObservationRegistration, ObservationSource,
};
use super::path_policy::is_machine_token;

const TARGET_ROOT: &str = "target/p1-gate/evidence";
const PACKAGE_DESCRIPTOR_ROOT: &str = "docs/reviews/evidence/p1/gate/descriptors";
const PACKAGE_DISTRIBUTION_ROOT: &str = "docs/reviews/evidence/p1/gate/distributions";
const PACKAGE_LOG_ROOT: &str = "docs/reviews/evidence/p1/gate/logs";

#[derive(Clone, Copy)]
enum EvidenceLocation {
    Target,
    Package,
}

struct RunLayout {
    location: EvidenceLocation,
    candidate: String,
    run: String,
}

struct DerivedRun {
    registry: RedactionRegistry,
    expected_paths: BTreeSet<RepositoryPath>,
}

pub(super) fn derive_target(
    checked: &OwnedSnapshot,
    run: &OwnedSnapshot,
) -> Result<RedactionRegistry, RedactionAuthoringError> {
    if run.is_empty() {
        return Err(RedactionAuthoringError::EmptyRunLocalAuthority);
    }
    let descriptors = run
        .iter()
        .filter(|(path, _)| {
            path.file_name() == GATE_DESCRIPTOR_NAME
                && path.as_str().starts_with("target/p1-gate/evidence/")
        })
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let [descriptor] = descriptors.as_slice() else {
        return Err(RedactionAuthoringError::InvalidRunLocalLayout);
    };
    let layout = RunLayout::from_descriptor(descriptor, EvidenceLocation::Target)?;
    let derived = derive_one(checked, run, &layout)?;
    if !run
        .iter()
        .all(|(path, _)| derived.expected_paths.contains(path))
        || run.len() != derived.expected_paths.len()
    {
        return Err(RedactionAuthoringError::InvalidRunLocalLayout);
    }
    Ok(derived.registry)
}

pub(super) fn derive_packaged(
    checked: &OwnedSnapshot,
) -> Result<Option<RedactionRegistry>, RedactionAuthoringError> {
    let descriptors = checked
        .iter()
        .filter(|(path, _)| {
            path.file_name() == GATE_DESCRIPTOR_NAME
                && is_beneath(path.as_str(), PACKAGE_DESCRIPTOR_ROOT)
        })
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    if descriptors.is_empty() {
        if checked.iter().any(|(path, _)| is_packaged_gate_path(path)) {
            return Err(RedactionAuthoringError::InvalidRunLocalLayout);
        }
        return Ok(None);
    }
    let [descriptor] = descriptors.as_slice() else {
        return Err(RedactionAuthoringError::InvalidRunLocalLayout);
    };
    let layout = RunLayout::from_descriptor(descriptor, EvidenceLocation::Package)?;
    let derived = derive_one(checked, checked, &layout)?;
    let actual = checked
        .iter()
        .filter(|(path, _)| is_packaged_gate_path(path))
        .map(|(path, _)| path)
        .collect::<BTreeSet<_>>();
    if actual.len() != derived.expected_paths.len()
        || !actual
            .iter()
            .all(|path| derived.expected_paths.contains(*path))
    {
        return Err(RedactionAuthoringError::InvalidRunLocalLayout);
    }
    Ok(Some(derived.registry))
}

pub(super) fn expected_descriptor(
    snapshot: &OwnedSnapshot,
    descriptor: &RepositoryPath,
) -> Result<ArtifactRegistration, RedactionAuthoringError> {
    let location = if is_beneath(descriptor.as_str(), TARGET_ROOT) {
        EvidenceLocation::Target
    } else if is_beneath(descriptor.as_str(), PACKAGE_DESCRIPTOR_ROOT) {
        EvidenceLocation::Package
    } else {
        return Err(RedactionAuthoringError::InvalidRunLocalLayout);
    };
    let layout = RunLayout::from_descriptor(descriptor, location)?;
    let derived = derive_one(snapshot, snapshot, &layout)?;
    derived
        .registry
        .artifacts()
        .find_map(|(path, artifact)| (path == descriptor).then(|| artifact.clone()))
        .ok_or(RedactionAuthoringError::InvalidGateDescriptor)
}

pub(super) fn is_packaged_gate_path(path: &RepositoryPath) -> bool {
    [
        PACKAGE_DESCRIPTOR_ROOT,
        PACKAGE_DISTRIBUTION_ROOT,
        PACKAGE_LOG_ROOT,
    ]
    .iter()
    .any(|root| is_beneath(path.as_str(), root))
}

fn derive_one(
    checked: &OwnedSnapshot,
    evidence: &OwnedSnapshot,
    layout: &RunLayout,
) -> Result<DerivedRun, RedactionAuthoringError> {
    let descriptor_path = layout.descriptor_path()?;
    let descriptor_bytes = regular_bytes(evidence, &descriptor_path)?;
    let document = decode_gate_run(checked, descriptor_bytes)
        .map_err(|_| RedactionAuthoringError::InvalidGateDescriptor)?;
    if document.candidate_commit != layout.candidate
        || !matches!(document.outcome.as_str(), "passed" | "failed")
        || !is_lower_hex(&document.candidate_tree, 40)
    {
        return Err(RedactionAuthoringError::InvalidGateDescriptor);
    }

    let mut artifacts = Vec::new();
    let mut expected_paths = BTreeSet::from([descriptor_path.clone()]);
    let mut gate_observations = Vec::new();
    for (offset, command) in document.commands.iter().enumerate() {
        let expected_order = offset
            .checked_add(1)
            .ok_or(RedactionAuthoringError::InvalidGateDescriptor)?;
        if command.order != expected_order {
            return Err(RedactionAuthoringError::InvalidGateDescriptor);
        }
        for stream in ["stderr", "stdout"] {
            let (record, path) = log_binding(layout, command, stream)?;
            let bytes = regular_bytes(evidence, &path)?;
            verify_structured_summary(bytes, command, stream)?;
            if record.bytes != bytes.len() || record.sha256 != digest_bytes(bytes) {
                return Err(RedactionAuthoringError::InvalidGateDescriptor);
            }
            expected_paths.insert(path.clone());
            gate_observations.push(ObservationRegistration::new(
                format!("gate-{expected_order:03}-{}-{stream}", command.id),
                path.clone(),
                ArtifactFamily::SanitizedLog,
                ObservationSource::LocalGate,
                Vec::new(),
                record.sha256,
            )?);
            artifacts.push(log_registration(path, bytes)?);
        }
        if command.kind == "distribution" {
            let sidecar = distribution_registration(layout, command, evidence)?;
            expected_paths.insert(sidecar.path().clone());
            artifacts.push(sidecar);
        } else if command.distribution.is_some() {
            return Err(RedactionAuthoringError::InvalidGateDescriptor);
        }
    }
    gate_observations.sort_by(|left, right| left.id().cmp(right.id()));
    artifacts.push(ArtifactRegistration::new(
        document.evidence_id,
        descriptor_path,
        ArtifactFamily::GateDescriptor,
        digest_bytes(descriptor_bytes),
        Vec::new(),
        gate_observations,
    )?);
    artifacts.sort_by(|left, right| left.path().cmp(right.path()));
    let registry = RedactionRegistry::new(artifacts, Vec::new())?;
    Ok(DerivedRun {
        registry,
        expected_paths,
    })
}

fn log_binding<'a>(
    layout: &RunLayout,
    command: &'a GateCommand,
    stream: &str,
) -> Result<(&'a super::gate_document::GateLogRecord, RepositoryPath), RedactionAuthoringError> {
    let name = format!("{:02}-{}.{stream}.log", command.order, command.id);
    let record = if stream == "stdout" {
        &command.stdout
    } else {
        &command.stderr
    };
    if record.path != name {
        return Err(RedactionAuthoringError::InvalidGateDescriptor);
    }
    Ok((record, layout.log_path(&name)?))
}

fn distribution_registration(
    layout: &RunLayout,
    command: &GateCommand,
    evidence: &OwnedSnapshot,
) -> Result<ArtifactRegistration, RedactionAuthoringError> {
    let stdout_path =
        layout.log_path(&format!("{:02}-{}.stdout.log", command.order, command.id))?;
    let sidecar_path = layout.distribution_path(&format!(
        "{:02}-{}.distribution.json",
        command.order, command.id
    ))?;
    let sidecar_bytes = regular_bytes(evidence, &sidecar_path)?;
    let document = crate::strict_json::decode_strict_json::<EvidenceDocument>(sidecar_bytes)
        .map_err(RedactionAuthoringError::InvalidRunLocalDocument)?;
    let [observation] = document.observations.as_slice() else {
        return Err(RedactionAuthoringError::InvalidRunLocalObservation);
    };
    let stdout_digest = digest_bytes(regular_bytes(evidence, &stdout_path)?);
    if document.schema_version != ArtifactFamily::Distribution.schema_version()
        || document.artifact_family != ArtifactFamily::Distribution
        || document.artifact_id != format!("p1-distribution-{}", command.id)
        || !document.synthetic_values.is_empty()
        || observation.id != format!("distribution-{}-stdout", command.id)
        || observation.referenced_path != stdout_path
        || observation.referenced_family != ArtifactFamily::SanitizedLog
        || observation.source != ObservationSource::LocalGate
        || !observation.synthetic_ids.is_empty()
        || observation.digest != stdout_digest
    {
        return Err(RedactionAuthoringError::InvalidRunLocalObservation);
    }
    let observation = ObservationRegistration::new(
        observation.id.clone(),
        observation.referenced_path.clone(),
        observation.referenced_family,
        observation.source,
        Vec::new(),
        observation.digest,
    )?;
    ArtifactRegistration::new(
        document.artifact_id,
        sidecar_path,
        ArtifactFamily::Distribution,
        digest_bytes(sidecar_bytes),
        Vec::new(),
        vec![observation],
    )
    .map_err(Into::into)
}

fn verify_structured_summary(
    bytes: &[u8],
    command: &GateCommand,
    stream: &str,
) -> Result<(), RedactionAuthoringError> {
    let fields = structured_fields(bytes)?;
    let matches = if stream == "stdout" {
        stdout_summary_matches(&fields, command)
    } else {
        stderr_summary_matches(&fields, command)
    };
    if !matches {
        return Err(RedactionAuthoringError::InvalidRunLocalObservation);
    }
    Ok(())
}

fn structured_fields(bytes: &[u8]) -> Result<BTreeMap<&str, &str>, RedactionAuthoringError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| RedactionAuthoringError::InvalidRunLocalObservation)?;
    let Some(line) = text.strip_suffix('\n') else {
        return Err(RedactionAuthoringError::InvalidRunLocalObservation);
    };
    if line.contains('\n') {
        return Err(RedactionAuthoringError::InvalidRunLocalObservation);
    }
    let mut fields = BTreeMap::new();
    for field in line.split_ascii_whitespace() {
        let Some((key, value)) = field.split_once('=') else {
            return Err(RedactionAuthoringError::InvalidRunLocalObservation);
        };
        if fields.insert(key, value).is_some() {
            return Err(RedactionAuthoringError::InvalidRunLocalObservation);
        }
    }
    Ok(fields)
}

fn stdout_summary_matches(fields: &BTreeMap<&str, &str>, command: &GateCommand) -> bool {
    let expected_result = if command.outcome == "passed" {
        "pass"
    } else {
        "fail"
    };
    let base_matches = parse_count(fields, "tests") == Some(command.test_executions)
        && fields.get("result") == Some(&expected_result);
    let counts_match = command.distribution.as_ref().map_or_else(
        || fields.len() == 2,
        |counts| {
            fields.len() == 4
                && counts.passed.checked_add(counts.failed) == Some(counts.observations)
                && parse_count(fields, "passed") == Some(counts.passed)
                && parse_count(fields, "failed") == Some(counts.failed)
        },
    );
    base_matches && counts_match
}

fn stderr_summary_matches(fields: &BTreeMap<&str, &str>, command: &GateCommand) -> bool {
    let expected_result = if command.process_outcome == "passed" {
        "pass"
    } else {
        "fail"
    };
    if fields.get("result") != Some(&expected_result) {
        return false;
    }
    match command.exit_code {
        Some(exit_code) if exit_code >= 0 => {
            fields.len() == 2
                && parse_i64(fields, "exit_status") == Some(exit_code)
        }
        Some(_) | None => fields.len() == 1,
    }
}

fn parse_count(fields: &BTreeMap<&str, &str>, key: &str) -> Option<u64> {
    let value = fields.get(key)?;
    match value.parse() {
        Ok(count) => Some(count),
        Err(_) => None,
    }
}

fn parse_i64(fields: &BTreeMap<&str, &str>, key: &str) -> Option<i64> {
    let value = fields.get(key)?;
    match value.parse() {
        Ok(number) => Some(number),
        Err(_) => None,
    }
}

fn log_registration(
    path: RepositoryPath,
    bytes: &[u8],
) -> Result<ArtifactRegistration, RedactionAuthoringError> {
    ArtifactRegistration::new(
        artifact_id(&path, ArtifactFamily::SanitizedLog),
        path,
        ArtifactFamily::SanitizedLog,
        digest_bytes(bytes),
        Vec::new(),
        Vec::new(),
    )
    .map_err(Into::into)
}

fn regular_bytes<'a>(
    snapshot: &'a OwnedSnapshot,
    path: &RepositoryPath,
) -> Result<&'a [u8], RedactionAuthoringError> {
    let Some(entry) = snapshot.get(path) else {
        return Err(RedactionAuthoringError::InvalidRunLocalLayout);
    };
    if entry.kind() != EntryKind::Regular {
        return Err(RedactionAuthoringError::NonRegularRunLocalArtifact);
    }
    Ok(entry.bytes())
}

impl RunLayout {
    fn from_descriptor(
        path: &RepositoryPath,
        location: EvidenceLocation,
    ) -> Result<Self, RedactionAuthoringError> {
        let root = match location {
            EvidenceLocation::Target => TARGET_ROOT,
            EvidenceLocation::Package => PACKAGE_DESCRIPTOR_ROOT,
        };
        let Some(suffix) = path.as_str().strip_prefix(&format!("{root}/")) else {
            return Err(RedactionAuthoringError::InvalidRunLocalLayout);
        };
        let components = suffix.split('/').collect::<Vec<_>>();
        let [candidate, run, file] = components.as_slice() else {
            return Err(RedactionAuthoringError::InvalidRunLocalLayout);
        };
        if *file != GATE_DESCRIPTOR_NAME
            || !is_lower_hex(candidate, 40)
            || !run.starts_with("run-")
            || !is_machine_token(run, 128)
        {
            return Err(RedactionAuthoringError::InvalidRunLocalLayout);
        }
        Ok(Self {
            location,
            candidate: (*candidate).to_owned(),
            run: (*run).to_owned(),
        })
    }

    fn descriptor_path(&self) -> Result<RepositoryPath, RedactionAuthoringError> {
        self.path(PACKAGE_DESCRIPTOR_ROOT, GATE_DESCRIPTOR_NAME)
    }

    fn log_path(&self, name: &str) -> Result<RepositoryPath, RedactionAuthoringError> {
        self.path(PACKAGE_LOG_ROOT, name)
    }

    fn distribution_path(&self, name: &str) -> Result<RepositoryPath, RedactionAuthoringError> {
        self.path(PACKAGE_DISTRIBUTION_ROOT, name)
    }

    fn path(
        &self,
        package_root: &str,
        name: &str,
    ) -> Result<RepositoryPath, RedactionAuthoringError> {
        let root = match self.location {
            EvidenceLocation::Target => TARGET_ROOT,
            EvidenceLocation::Package => package_root,
        };
        RepositoryPath::parse(format!("{root}/{}/{}/{name}", self.candidate, self.run))
            .map_err(|_| RedactionAuthoringError::InvalidRunLocalLayout)
    }
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn is_beneath(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|suffix| suffix.starts_with('/') && suffix.len() > 1)
}
