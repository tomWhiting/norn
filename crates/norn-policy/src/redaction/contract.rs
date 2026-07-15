use serde_json::{Map, Value};

use crate::digest::digest_bytes;
use crate::strict_json::decode_strict_json;

use super::model::ArtifactRegistration;
use super::scan::decoded_structural_violation;
use super::validate::{ArtifactIssue, RedactionCode};

const CONTRACT_SCHEMA_KEYS: &[&str] = &["$defs", "$id", "$schema", "oneOf"];
const INVENTORY_KEYS: &[&str] = &[
    "annotation_variants",
    "cache_controls",
    "compaction_item",
    "compaction_trigger",
    "endpoint_contracts",
    "endpoint_examples_may_omit_cache_write_tokens",
    "include_values",
    "incomplete_reasons",
    "input_variants",
    "kind",
    "output_variants",
    "reasoning_item",
    "response_statuses",
    "schema_version",
    "tool_variants",
    "usage_paths",
    "usage_schema_requires_cache_write_tokens",
];
const MANIFEST_KEYS: &[&str] = &[
    "api_description_version",
    "extractor_version",
    "kind",
    "openapi_version",
    "outputs",
    "retrieved_on",
    "schema_version",
    "sources",
];
const GRAPH_KEYS: &[&str] = &[
    "kind",
    "nodes",
    "root_source_key",
    "schema",
    "schema_version",
];
const DISCREPANCY_KEYS: &[&str] = &[
    "count",
    "gate_corrections",
    "items",
    "kind",
    "schema_version",
];
const SSE_KEYS: &[&str] = &["events", "kind", "schema_version"];
const EVIDENCE_SCHEMA_KEYS: &[&str] = &[
    "$defs",
    "$id",
    "$schema",
    "additionalProperties",
    "allOf",
    "properties",
    "required",
    "title",
    "type",
];
const GATE_COMMAND_KEYS: &[&str] = &[
    "base_commit",
    "commands",
    "implementation",
    "phase",
    "resource_limits",
    "schema_version",
];
const BASE_AUTHORITY_KEYS: &[&str] = &[
    "algorithm",
    "analysis_projection",
    "analysis_snapshot_identity",
    "commit",
    "domain",
    "entry_count",
    "framing",
    "generated_include_registry",
    "generated_include_registry_domain",
    "generated_include_registry_framing",
    "generated_include_registry_identity",
    "git_inventory_domain",
    "git_inventory_identity",
    "mode_counts",
    "schema_version",
    "tree",
];

#[derive(Clone, Copy)]
enum DocumentShape {
    JsonSchema {
        id: &'static str,
        keys: &'static [&'static str],
    },
    Kind {
        kind: &'static str,
        keys: &'static [&'static str],
    },
    BaseAuthority,
    GateCommands,
}

#[derive(Clone, Copy)]
struct ContractDocument {
    path: &'static str,
    sha256: &'static str,
    shape: DocumentShape,
}

const DOCUMENTS: &[ContractDocument] = &[
    ContractDocument {
        path: "crates/norn-policy/tests/evidence/p1_base_authority.json",
        sha256: "e8fe5d371854f0350bc8483dcdf8b9ec3e3c1b13e95207b47d1562766e190e02",
        shape: DocumentShape::BaseAuthority,
    },
    ContractDocument {
        path: "policy/contracts/openai-responses-v1/contract.schema.json",
        sha256: "52513731eb5885ef8d894e1f14390f8ad8abe7ef9f98de49b3421d166b650cf9",
        shape: DocumentShape::JsonSchema {
            id: "https://norn.invalid/policy/contracts/openai-responses-v1/contract.schema.json",
            keys: CONTRACT_SCHEMA_KEYS,
        },
    },
    ContractDocument {
        path: "policy/contracts/openai-responses-v1/inventories.json",
        sha256: "b02fa7d1a05f884bcc06f60ff0f8147422a2939c971cdb1e37ea693fe5c42547",
        shape: DocumentShape::Kind {
            kind: "public_responses_inventories",
            keys: INVENTORY_KEYS,
        },
    },
    ContractDocument {
        path: "policy/contracts/openai-responses-v1/manifest.json",
        sha256: "b430fa4c864b68c99b8b0dd3fe1e31c60ec68142cc92aa72ca2e1696f956e98d",
        shape: DocumentShape::Kind {
            kind: "public_responses_contract_manifest",
            keys: MANIFEST_KEYS,
        },
    },
    ContractDocument {
        path: "policy/contracts/openai-responses-v1/request-graph.json",
        sha256: "caa5e1bd184fc7a5b79227a1f36371b1a990a2424c2484806de88be375cf7520",
        shape: DocumentShape::Kind {
            kind: "public_responses_request_graph",
            keys: GRAPH_KEYS,
        },
    },
    ContractDocument {
        path: "policy/contracts/openai-responses-v1/response-graph.json",
        sha256: "22e7b7b4e7941fe7f968951c8704645fa9bf59fd7b10cbc7a9a7bfa04b223fc9",
        shape: DocumentShape::Kind {
            kind: "public_responses_response_graph",
            keys: GRAPH_KEYS,
        },
    },
    ContractDocument {
        path: "policy/contracts/openai-responses-v1/source-discrepancies.json",
        sha256: "086b2095875da350e9888ecb24c2c94804eff92426c5d6b1f1827cfcf6d877f7",
        shape: DocumentShape::Kind {
            kind: "public_responses_source_discrepancies",
            keys: DISCREPANCY_KEYS,
        },
    },
    ContractDocument {
        path: "policy/contracts/openai-responses-v1/sse-events.json",
        sha256: "d10d92fa6ecc3bfa52add80d15489d39697f613fe298297fc283005883a6ff2c",
        shape: DocumentShape::Kind {
            kind: "public_responses_sse_events",
            keys: SSE_KEYS,
        },
    },
    ContractDocument {
        path: "policy/evidence-schemas/gate-run.schema.json",
        sha256: "c777b20a9bbb98338702ee4b281fad07d08828ece6867d31b3db8e874ca3d72d",
        shape: DocumentShape::JsonSchema {
            id: "https://norn.example.invalid/policy/evidence-schemas/gate-run.schema.json",
            keys: EVIDENCE_SCHEMA_KEYS,
        },
    },
    ContractDocument {
        path: "policy/gate-commands.json",
        sha256: "34a4ed9debd7381daaf7e208c8a3abc80c6b188c5ddedef660a4075f47edc555",
        shape: DocumentShape::GateCommands,
    },
];

pub(crate) fn authorities() -> impl ExactSizeIterator<Item = (&'static str, &'static str)> {
    DOCUMENTS
        .iter()
        .map(|document| (document.path, document.sha256))
}

pub(crate) fn validate_contract_document(
    registration: &ArtifactRegistration,
    bytes: &[u8],
    issues: &mut Vec<ArtifactIssue>,
) {
    let Some(expected) = DOCUMENTS
        .iter()
        .find(|document| document.path == registration.path().as_str())
    else {
        issues.push(issue(RedactionCode::SchemaMismatch));
        return;
    };
    let Ok(value) = decode_strict_json::<Value>(bytes) else {
        issues.push(issue(RedactionCode::InvalidJson));
        return;
    };
    scan_decoded_value(&value, issues);
    if digest_bytes(bytes).to_hex() != expected.sha256 || !expected.shape.matches(&value) {
        issues.push(issue(RedactionCode::SchemaMismatch));
    }
}

impl DocumentShape {
    fn matches(self, value: &Value) -> bool {
        let Some(object) = value.as_object() else {
            return false;
        };
        match self {
            Self::JsonSchema { id, keys } => {
                exact_keys(object, keys)
                    && object.get("$id").and_then(Value::as_str) == Some(id)
                    && object.get("$schema").and_then(Value::as_str)
                        == Some("https://json-schema.org/draft/2020-12/schema")
            }
            Self::Kind { kind, keys } => {
                exact_keys(object, keys)
                    && object.get("kind").and_then(Value::as_str) == Some(kind)
                    && object.get("schema_version").and_then(Value::as_u64) == Some(1)
            }
            Self::BaseAuthority => {
                exact_keys(object, BASE_AUTHORITY_KEYS)
                    && object.get("schema_version").and_then(Value::as_u64) == Some(1)
                    && object.get("algorithm").and_then(Value::as_str) == Some("sha256")
                    && object.get("commit").and_then(Value::as_str)
                        == Some(crate::baseline::P1_BASE_COMMIT)
                    && object.get("tree").and_then(Value::as_str)
                        == Some(crate::baseline::P1_BASE_TREE)
                    && digest_field_matches(
                        object,
                        "analysis_snapshot_identity",
                        crate::baseline::P1_BASE_ANALYSIS_SNAPSHOT_IDENTITY,
                    )
                    && digest_field_matches(
                        object,
                        "generated_include_registry_identity",
                        crate::baseline::P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
                    )
                    && object.get("git_inventory_domain").and_then(Value::as_str)
                        == Some("norn-policy-p1-git-tree-inventory-1")
                    && digest_field_matches(
                        object,
                        "git_inventory_identity",
                        crate::P1_BASE_GIT_INVENTORY_IDENTITY,
                    )
            }
            Self::GateCommands => {
                exact_keys(object, GATE_COMMAND_KEYS)
                    && object.get("schema_version").and_then(Value::as_u64) == Some(1)
                    && object.get("phase").and_then(Value::as_str) == Some("P1")
                    && object.get("base_commit").and_then(Value::as_str)
                        == Some(crate::baseline::P1_BASE_COMMIT)
            }
        }
    }
}

fn exact_keys(object: &Map<String, Value>, expected: &[&str]) -> bool {
    object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
}

fn digest_field_matches(object: &Map<String, Value>, key: &str, expected: crate::Digest) -> bool {
    let Some(value) = object.get(key).and_then(Value::as_str) else {
        return false;
    };
    let Ok(actual) = value.parse::<crate::Digest>() else {
        return false;
    };
    actual == expected
}

fn scan_decoded_value(value: &Value, issues: &mut Vec<ArtifactIssue>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                scan_string(key, issues);
                scan_decoded_value(child, issues);
            }
        }
        Value::Array(values) => {
            for child in values {
                scan_decoded_value(child, issues);
            }
        }
        Value::String(value) => scan_string(value, issues),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn scan_string(value: &str, issues: &mut Vec<ArtifactIssue>) {
    if let Some(code) = decoded_structural_violation(value) {
        issues.push(issue(code.into()));
    }
}

const fn issue(code: RedactionCode) -> ArtifactIssue {
    ArtifactIssue::new(None, code)
}
