//! Live production-adapter proof for deterministic P1 review authoring.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::{error::Error, io};

use norn::repository_snapshot::RepositorySnapshotAdapter;
use norn_policy::authoring::P1ReviewCandidate;
use norn_policy::baseline::OriginLedger;
use norn_policy::facts::analyze_facts;
use norn_policy::rust::modules::GeneratedIncludeRegistry;
use norn_policy::strict_json::decode_strict_json;
use norn_policy::{CompleteCurrentSnapshot, EntryKind, P1BaseSnapshot, RepositoryPath};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn live_repository_derives_a_deterministic_p1_review_candidate() -> TestResult {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let adapter = RepositorySnapshotAdapter::discover(manifest_dir)?;
    let current = adapter.acquire_current()?;
    let base = adapter.acquire_p1_base()?;
    verify_base_writer_inventory(&base, current.snapshot())?;
    let candidate = P1ReviewCandidate::derive(&base, current.snapshot())?;

    let first_origin = candidate.encode_origin()?;
    let first_inventory = candidate.encode_inventory()?;
    let decoded_origin = OriginLedger::decode_p1(&first_origin)?;
    assert_eq!(first_origin, candidate.encode_origin()?);
    assert_eq!(first_inventory, candidate.encode_inventory()?);
    assert_eq!(&decoded_origin, candidate.origin());
    assert!(first_origin.ends_with(b"\n"));
    assert!(first_inventory.ends_with(b"\n"));
    assert!(!candidate.inventory().writer_operations().is_empty());
    adapter.revalidate_current(&current)?;
    Ok(())
}

fn verify_base_writer_inventory(
    base: &P1BaseSnapshot,
    current: &CompleteCurrentSnapshot,
) -> TestResult {
    let registry_path = RepositoryPath::parse("policy/generated-includes.json")?;
    let registry_entry = current
        .snapshot()
        .get(&registry_path)
        .ok_or_else(|| io::Error::other("generated-include registry is missing"))?;
    if registry_entry.kind() != EntryKind::Regular {
        return Err(io::Error::other("generated-include registry is not regular").into());
    }
    let registry = decode_strict_json::<GeneratedIncludeRegistry>(registry_entry.bytes())?;
    let facts = analyze_facts(base.snapshot(), &registry);
    let writers = facts.writers().ok_or_else(|| {
        io::Error::other(format!(
            "immutable-base fact analysis failed: {:?}",
            facts.failures()
        ))
    })?;
    if writers.is_registry_complete() {
        return Ok(());
    }
    let mut unknown_counts = BTreeMap::new();
    let mut unknown_paths = BTreeSet::new();
    for unknown in writers.unknowns() {
        *unknown_counts
            .entry((unknown.reason(), unknown.candidate().as_str()))
            .or_insert(0_usize) += 1;
        unknown_paths.insert(unknown.path().as_str());
    }
    let unobserved = writers
        .unobserved_required_sinks()
        .iter()
        .map(|sink| sink.as_str())
        .collect::<Vec<_>>();
    Err(io::Error::other(format!(
        "immutable-base writer inventory is incomplete: unknown_count={} unknown_path_count={} unknown_counts={unknown_counts:?} unobserved={unobserved:?}",
        writers.unknowns().len(),
        unknown_paths.len(),
    ))
    .into())
}
