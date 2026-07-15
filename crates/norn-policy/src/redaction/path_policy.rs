use crate::path::RepositoryPath;

use super::model::{ArtifactFamily, RegistrationError};

pub(crate) const GOVERNED_ROOTS: &[&str] = &[
    "crates/norn/testdata/openai_responses",
    "docs/reviews/evidence/p1",
    "policy/contracts/openai-responses-v1",
    "policy/evidence-schemas",
    "target/p1-gate/evidence",
];

pub(crate) const GOVERNED_SCRIPT_PATHS: &[&str] = &[
    "scripts/p1-added-line-audit",
    "scripts/p1-distributions",
    "scripts/p1-gate",
    "scripts/p1-gate-self-check",
    "scripts/p1-redaction-check",
    "scripts/p1_gate.py",
    "scripts/p1_gate_contract.py",
    "scripts/p1_gate_environment.py",
    "scripts/p1_gate_evidence.py",
    "scripts/p1_gate_runtime.py",
    "scripts/p1_json_schema.py",
    "scripts/p1_origin_evidence.py",
    "scripts/test_p1_gate_contract.py",
    "scripts/test_p1_gate_environment.py",
    "scripts/test_p1_gate_evidence.py",
    "scripts/test_p1_gate_runtime.py",
    "scripts/test_p1_json_schema.py",
    "scripts/test_p1_origin_evidence.py",
];

pub(crate) const GOVERNED_EVIDENCE_TOOL_PATHS: &[&str] = &[
    "docs/reviews/evidence/p1/openai_contract_build.py",
    "docs/reviews/evidence/p1/openai_contract_constants.py",
    "docs/reviews/evidence/p1/openai_contract_extract.py",
    "docs/reviews/evidence/p1/openai_contract_graph.py",
    "docs/reviews/evidence/p1/responses_fixture_codex.py",
    "docs/reviews/evidence/p1/responses_fixture_generate.py",
    "docs/reviews/evidence/p1/responses_fixture_public_requests.py",
    "docs/reviews/evidence/p1/responses_fixture_public_streams.py",
    "docs/reviews/evidence/p1/responses_fixture_types.py",
    "docs/reviews/evidence/p1/test_openai_contract_extract.py",
    "docs/reviews/evidence/p1/test_responses_fixture_generate.py",
];

pub(crate) const GOVERNED_CONTRACT_PATHS: &[&str] = &[
    "crates/norn-policy/tests/evidence/p1_base_authority.json",
    "policy/generated-includes.json",
    "policy/gate-commands.json",
];

pub(crate) const PROVENANCE_ROOTS: &[&str] = &[
    "crates/norn-policy",
    "crates/norn/testdata/openai_responses",
    "docs/reviews/evidence/p1",
    "scripts",
];

struct EvidenceToolPaths {
    index: usize,
}

impl Iterator for EvidenceToolPaths {
    type Item = &'static str;

    fn next(&mut self) -> Option<Self::Item> {
        let value = if self.index < GOVERNED_SCRIPT_PATHS.len() {
            GOVERNED_SCRIPT_PATHS.get(self.index).copied()
        } else {
            GOVERNED_EVIDENCE_TOOL_PATHS
                .get(self.index - GOVERNED_SCRIPT_PATHS.len())
                .copied()
        };
        if value.is_some() {
            self.index += 1;
        }
        value
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.len();
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for EvidenceToolPaths {
    fn len(&self) -> usize {
        GOVERNED_SCRIPT_PATHS.len() + GOVERNED_EVIDENCE_TOOL_PATHS.len() - self.index
    }
}

pub(crate) fn evidence_tool_paths() -> impl ExactSizeIterator<Item = &'static str> {
    EvidenceToolPaths { index: 0 }
}

pub(crate) fn is_evidence_tool_candidate(path: &RepositoryPath) -> bool {
    let value = path.as_str();
    if let Some(name) = value.strip_prefix("scripts/") {
        return !name.contains('/')
            && (name.starts_with("p1-")
                || name.starts_with("p1_")
                || name.starts_with("test_p1_"));
    }
    value
        .strip_prefix("docs/reviews/evidence/p1/")
        .is_some_and(|name| !name.contains('/') && name.ends_with(".py"))
}

pub(crate) fn validate_artifact_path(
    path: &RepositoryPath,
    family: ArtifactFamily,
) -> Result<(), RegistrationError> {
    if !is_machine_path(path) || !family_accepts_path(path, family) {
        return Err(RegistrationError::InvalidArtifactPath);
    }
    Ok(())
}

pub(crate) fn is_authority_path(path: &RepositoryPath, roots: &[&str]) -> bool {
    is_machine_path(path)
        && roots
            .iter()
            .any(|root| path_is_beneath(path.as_str(), root))
}

pub(crate) fn validate_sorted_ids(ids: &[String]) -> Result<(), RegistrationError> {
    for id in ids {
        validate_machine_id(id)?;
    }
    if !ids.windows(2).all(|pair| pair[0] < pair[1]) {
        return Err(RegistrationError::UnstableAuthorityOrder);
    }
    Ok(())
}

pub(crate) fn validate_machine_id(value: &str) -> Result<(), RegistrationError> {
    if !is_machine_token(value, 128) {
        return Err(RegistrationError::InvalidMachineId);
    }
    Ok(())
}

pub(crate) fn is_machine_token(value: &str, limit: usize) -> bool {
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    value.len() <= limit
        && (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b':' | b'-')
        })
}

fn is_machine_path(path: &RepositoryPath) -> bool {
    path.as_str().split('/').all(|component| {
        !component.is_empty()
            && component.len() <= 128
            && component.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
    })
}

fn family_accepts_path(path: &RepositoryPath, family: ArtifactFamily) -> bool {
    let value = path.as_str();
    match family {
        ArtifactFamily::ProtocolFixture => {
            path_is_beneath(value, "crates/norn/testdata/openai_responses")
                && (has_exact_extension(value, "json") || has_exact_extension(value, "sse"))
        }
        ArtifactFamily::TraceabilityJsonl => {
            value == "docs/reviews/evidence/p1/finding-traceability.jsonl"
        }
        ArtifactFamily::ContractSchema => {
            (path_is_beneath(value, "policy/contracts/openai-responses-v1")
                || path_is_beneath(value, "policy/evidence-schemas"))
                && has_exact_extension(value, "json")
                || GOVERNED_CONTRACT_PATHS.contains(&value)
        }
        ArtifactFamily::GateDescriptor | ArtifactFamily::Distribution => {
            (path_is_beneath(value, "target/p1-gate/evidence")
                && has_exact_extension(value, "json"))
                || packaged_json_path(value, family)
        }
        ArtifactFamily::SanitizedLog => {
            path_is_beneath(value, "target/p1-gate/evidence")
                || path_is_beneath(value, "docs/reviews/evidence/p1/gate/logs")
        }
        ArtifactFamily::EvidenceToolSource => {
            GOVERNED_EVIDENCE_TOOL_PATHS.contains(&value) || GOVERNED_SCRIPT_PATHS.contains(&value)
        }
    }
}

fn packaged_json_path(value: &str, family: ArtifactFamily) -> bool {
    let root = match family {
        ArtifactFamily::GateDescriptor => "docs/reviews/evidence/p1/gate/descriptors",
        ArtifactFamily::Distribution => "docs/reviews/evidence/p1/gate/distributions",
        ArtifactFamily::ProtocolFixture
        | ArtifactFamily::TraceabilityJsonl
        | ArtifactFamily::ContractSchema
        | ArtifactFamily::SanitizedLog
        | ArtifactFamily::EvidenceToolSource => return false,
    };
    path_is_beneath(value, root) && has_exact_extension(value, "json")
}

pub(crate) fn has_exact_extension(path: &str, expected: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, extension)| extension == expected)
}

fn path_is_beneath(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|suffix| suffix.starts_with('/') && suffix.len() > 1)
}
