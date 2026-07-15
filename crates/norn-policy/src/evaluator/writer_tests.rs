use std::collections::BTreeSet;
use std::error::Error;
use std::io;

use super::evaluate_with_fixture_authorities;
use super::state::AuthorityView;
use crate::baseline::{
    LegacyGovernance, OriginLedger, P1_BASE_COMMIT, P1_BASE_TREE,
    P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY, RepositoryBaselineFacts,
};
use crate::config::RepositoryPolicy;
use crate::facts::analyze_facts;
use crate::finding::FindingCode;
use crate::phase_lock::CampaignPhase;
use crate::redaction::RedactionRegistry;
use crate::rust::modules::GeneratedIncludeRegistry;
use crate::version::{ANALYZER_VERSION, DIGEST_VERSION};
use crate::writers::{
    WRITER_ANALYZER_VERSION, WRITER_SCHEMA_VERSION, WriterFamilyRegistry, WriterOperation,
    WriterOperationId, builtin_sink_registry,
};
use crate::{EntryKind, OwnedSnapshot, PolicyState, RepositoryPath, SnapshotEntry, digest_bytes};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const MANIFEST: &[u8] =
    b"[workspace]\n[package]\nname = \"fixture\"\nedition = \"2024\"\nbuild = false\n";

#[test]
fn writer_registry_covers_historical_and_current_union() -> TestResult {
    let base = snapshot(
        b"pub fn persist() { let result = std::fs::write(\"base.txt\", b\"x\"); drop(result); }\n",
    )?;
    let current = snapshot(
        b"pub fn persist() { let result = std::fs::write(\"current.txt\", b\"x\"); drop(result); }\n",
    )?;
    let generated = GeneratedIncludeRegistry::empty();
    let policy = policy()?;
    let origin = origin(&base, &generated, &policy)?;
    let base_facts = analyze_facts(&base, &generated);
    let current_facts = analyze_facts(&current, &generated);
    let base_writers = base_facts
        .writers()
        .ok_or_else(|| io::Error::other("base writer inventory is unavailable"))?;
    let current_writers = current_facts
        .writers()
        .ok_or_else(|| io::Error::other("current writer inventory is unavailable"))?;
    let operations = base_writers
        .operations()
        .iter()
        .chain(current_writers.operations())
        .map(WriterOperation::id)
        .collect::<BTreeSet<_>>();
    assert_eq!(operations.len(), 2);
    let writers = writer_registry(&operations)?;
    assert!(writers.validate_against_origin(&origin).is_empty());

    let governance = empty_governance()?;
    let redaction = RedactionRegistry::new(Vec::new(), Vec::new())?;
    let state = evaluate_with_fixture_authorities(
        &current,
        AuthorityView {
            repository_policy: &policy,
            generated_includes: &generated,
            origin: &origin,
            governance: &governance,
            writer_families: &writers,
            redaction: &redaction,
            active_phase: CampaignPhase::P1,
        },
    );
    let PolicyState::Ready(report) = state else {
        return Err(io::Error::other("writer-union evaluation was not ready").into());
    };
    assert!(
        !report
            .findings()
            .iter()
            .any(|finding| finding.code() == FindingCode::WriterClassification)
    );
    Ok(())
}

fn snapshot(source: &[u8]) -> TestResult<OwnedSnapshot> {
    Ok(OwnedSnapshot::try_from_entries([
        (
            RepositoryPath::parse("Cargo.toml")?,
            SnapshotEntry::copy_from_slice(EntryKind::Regular, MANIFEST),
        ),
        (
            RepositoryPath::parse("src/lib.rs")?,
            SnapshotEntry::copy_from_slice(EntryKind::Regular, source),
        ),
    ])?)
}

fn policy() -> TestResult<RepositoryPolicy> {
    let document = format!(
        "schema_version = 1\n\n[algorithms]\nanalyzer = \"{ANALYZER_VERSION}\"\ndigest = \"{DIGEST_VERSION}\"\n\n[production_loc]\nentrypoint_max = 200\nother_rust_max = 500\n"
    );
    Ok(RepositoryPolicy::decode(document.as_bytes())?)
}

fn origin(
    base: &OwnedSnapshot,
    generated: &GeneratedIncludeRegistry,
    policy: &RepositoryPolicy,
) -> TestResult<OriginLedger> {
    let facts = analyze_facts(base, generated);
    let baseline = RepositoryBaselineFacts::try_from_repository(&facts)?;
    let document = serde_json::json!({
        "schema_version": 1,
        "algorithms": {
            "analyzer": ANALYZER_VERSION,
            "digest": DIGEST_VERSION,
        },
        "base": {
            "commit": P1_BASE_COMMIT,
            "tree": P1_BASE_TREE,
        },
        "digests": {
            "repository_policy": policy.normalized_digest()?,
            "source_inventory": baseline.source_inventory_digest(),
            "generated_include_registry": P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY,
        },
        "source_inventory": baseline.source_inventory(),
        "compile_test_fixtures": baseline.compile_test_fixtures(),
        "production_files": baseline.production_files(),
        "item_groups": baseline.item_groups(),
        "prohibited_debt": baseline.prohibited_debt(),
        "writer_operations": baseline.writer_operations(),
    });
    Ok(OriginLedger::decode_p1(&serde_json::to_vec(&document)?)?)
}

fn writer_registry(operations: &BTreeSet<WriterOperationId>) -> TestResult<WriterFamilyRegistry> {
    let sink = builtin_sink_registry()?.digest();
    let resolutions = digest_bytes(b"fixture-writer-resolutions");
    let families = if operations.is_empty() {
        "[]"
    } else {
        "[\"session\"]"
    };
    let mut document = format!(
        "schema_version = {WRITER_SCHEMA_VERSION}\nsink_registry = \"{sink}\"\nwriter_resolutions = \"{resolutions}\"\n\n[algorithms]\nwriter = \"{WRITER_ANALYZER_VERSION}\"\ndigest = \"{DIGEST_VERSION}\"\n\n[vocabulary]\nfamilies = {families}\nshared_primitives = []\ncleanup_reviews = []\nfalse_positive_reviews = []\n"
    );
    for operation in operations {
        document.push_str("\n[[classifications]]\noperation = \"");
        document.push_str(&operation.digest().to_string());
        document.push_str("\"\nclassification = { class = \"family\", family = \"session\" }\n");
    }
    Ok(WriterFamilyRegistry::decode_p1(document.as_bytes())?)
}

fn empty_governance() -> TestResult<LegacyGovernance> {
    let document = format!(
        "schema_version = 1\nloc_exceptions = []\ndebt_exceptions = []\n\n[algorithms]\nanalyzer = \"{ANALYZER_VERSION}\"\ndigest = \"{DIGEST_VERSION}\"\n"
    );
    Ok(LegacyGovernance::decode(document.as_bytes())?)
}
