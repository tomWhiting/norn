use std::error::Error;
use std::io;

use norn_policy::baseline::{
    CurrentRepositoryFacts, ItemGroupFact, LegacyGovernance, LocCeilings, OriginLedger,
    P1_BASE_COMMIT, P1_BASE_TREE, P1_GENERATED_INCLUDE_TECHNICAL_IDENTITY, RepositoryBaselineFacts,
};
use norn_policy::digest::Digest;
use norn_policy::facts::{self, analyze_facts};
use norn_policy::path::RepositoryPath;
use norn_policy::rust::ProductionMetrics;
use norn_policy::rust::modules::{
    GeneratedIncludeRegistry, ModuleTargetIdentity, ModuleTargetKind,
};
use norn_policy::version::{ANALYZER_VERSION, DIGEST_VERSION};
use norn_policy::{OwnedSnapshot, SnapshotEntry};

pub(super) type TestResult<T = ()> = Result<T, Box<dyn Error>>;

pub(super) fn digest(byte: u8) -> Digest {
    Digest::from_bytes([byte; 32])
}

pub(super) fn canonical_production(
    path: &str,
    loc: u64,
    hash: u8,
    targets: Vec<ModuleTargetIdentity>,
) -> TestResult<facts::ProductionFileFact> {
    Ok(facts::ProductionFileFact {
        path: RepositoryPath::parse(path)?,
        targets,
        metrics: ProductionMetrics {
            loc,
            projection: digest(hash),
            excluded: Vec::new(),
        },
        module_shape: Vec::new(),
    })
}

pub(super) fn target(
    package: &str,
    name: &str,
    kind: ModuleTargetKind,
    root: &str,
) -> TestResult<ModuleTargetIdentity> {
    Ok(ModuleTargetIdentity {
        package: package.to_owned(),
        package_root: Some(RepositoryPath::parse("crates/sample")?),
        kind,
        name: name.to_owned(),
        root: RepositoryPath::parse(root)?,
    })
}

pub(super) fn item_group(
    path: &str,
    base_identity: u8,
    content: u8,
    production_count: u32,
    test_only_count: u32,
) -> TestResult<ItemGroupFact> {
    Ok(ItemGroupFact::new(
        RepositoryPath::parse(path)?,
        digest(base_identity),
        digest(content),
        production_count,
        test_only_count,
    )?)
}

pub(super) fn origin() -> TestResult<OriginLedger> {
    let baseline = origin_baseline()?;
    decoded_origin_fixture(digest(10), &baseline)
}

pub(super) fn origin_baseline() -> TestResult<RepositoryBaselineFacts> {
    baseline_from_legacy(&legacy_source(510, 7, true), None)
}

pub(super) fn current_legacy(
    line_count: usize,
    value: u32,
    include_debt: bool,
) -> TestResult<CurrentRepositoryFacts> {
    let baseline = baseline_from_legacy(&legacy_source(line_count, value, include_debt), None)?;
    Ok(CurrentRepositoryFacts::from_baseline(&baseline))
}

pub(super) fn current_with_new_debt() -> TestResult<CurrentRepositoryFacts> {
    let extra = legacy_source(510, 9, true);
    let baseline = baseline_from_legacy(&legacy_source(510, 7, true), Some(&extra))?;
    Ok(CurrentRepositoryFacts::from_baseline(&baseline))
}

pub(super) fn isolated_debt_origin() -> TestResult<OriginLedger> {
    let baseline = isolated_debt_baseline(true)?;
    decoded_origin_fixture(digest(10), &baseline)
}

// Decoder/evaluator fixtures may model synthetic facts, but production origin
// generation only accepts the opaque exact-base proof.
pub(super) fn decoded_origin_fixture(
    repository_policy: Digest,
    baseline: &RepositoryBaselineFacts,
) -> TestResult<OriginLedger> {
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
            "repository_policy": repository_policy,
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

pub(super) fn isolated_debt_current(include_debt: bool) -> TestResult<CurrentRepositoryFacts> {
    let baseline = isolated_debt_baseline(include_debt)?;
    Ok(CurrentRepositoryFacts::from_baseline(&baseline))
}

pub(super) fn baseline_from_sources(files: &[(&str, &str)]) -> TestResult<RepositoryBaselineFacts> {
    baseline_from_manifest(manifest(), files)
}

pub(super) fn baseline_from_manifest(
    manifest: &str,
    files: &[(&str, &str)],
) -> TestResult<RepositoryBaselineFacts> {
    let mut entries = vec![("Cargo.toml", manifest)];
    entries.extend_from_slice(files);
    let snapshot = snapshot(&entries)?;
    let facts = analyze_facts(&snapshot, &GeneratedIncludeRegistry::empty());
    Ok(RepositoryBaselineFacts::try_from_repository(&facts)?)
}

fn baseline_from_legacy(legacy: &str, extra: Option<&str>) -> TestResult<RepositoryBaselineFacts> {
    let root = if extra.is_some() {
        "mod legacy;\nmod new_debt;\n"
    } else {
        "mod legacy;\n"
    };
    let mut files = vec![("src/lib.rs", root), ("src/legacy.rs", legacy)];
    if let Some(extra) = extra {
        files.push(("src/new_debt.rs", extra));
    }
    baseline_from_sources(&files)
}

fn isolated_debt_baseline(include_debt: bool) -> TestResult<RepositoryBaselineFacts> {
    let legacy = legacy_source(510, 7, false);
    let debt = if include_debt {
        format!(
            "pub fn legacy_debt() {{ {}(\"legacy debt\"); }}\n",
            prohibited_macro_name()
        )
    } else {
        "pub fn legacy_debt() {}\n".to_owned()
    };
    baseline_from_sources(&[
        ("src/lib.rs", "mod legacy;\nmod legacy_debt;\n"),
        ("src/legacy.rs", &legacy),
        ("src/legacy_debt.rs", &debt),
    ])
}

fn legacy_source(line_count: usize, value: u32, include_debt: bool) -> String {
    let mut source = String::new();
    for index in 0..line_count {
        source.push_str("pub const VALUE_");
        source.push_str(&index.to_string());
        source.push_str(": u32 = ");
        source.push_str(&value.to_string());
        source.push_str(";\n");
    }
    source.push_str(
        "pub fn save() { let result = std::fs::write(\"artifact\", b\"data\"); drop(result); }\n",
    );
    if include_debt {
        source.push_str("pub fn legacy_debt() { ");
        source.push_str(&prohibited_macro_name());
        source.push_str("(\"legacy debt\"); }\n");
    }
    source
}

fn manifest() -> &'static str {
    "[workspace]\n[package]\nname = \"sample\"\nedition = \"2024\"\nbuild = false\n"
}

fn snapshot(entries: &[(&str, &str)]) -> TestResult<OwnedSnapshot> {
    let entries = entries
        .iter()
        .map(|(path, contents)| {
            Ok((
                RepositoryPath::parse(*path)?,
                SnapshotEntry::regular(contents.as_bytes().to_vec()),
            ))
        })
        .collect::<TestResult<Vec<_>>>()?;
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}

fn prohibited_macro_name() -> String {
    ["pan", "ic!"].concat()
}

pub(super) fn limits() -> TestResult<LocCeilings> {
    Ok(LocCeilings::new(200, 500)?)
}

pub(super) fn governance_document(
    origin: &OriginLedger,
    loc_state: &str,
    debt_state: &str,
    due_phase: &str,
) -> TestResult<String> {
    let loc = origin
        .production_files()
        .iter()
        .find(|fact| fact.production_loc() > 500)
        .ok_or_else(|| missing("production fact"))?;
    let debt = origin
        .prohibited_debt()
        .first()
        .ok_or_else(|| missing("debt fact"))?;
    Ok(format!(
        r#"schema_version = 1

[algorithms]
analyzer = "norn-policy-1"
digest = "norn-sha256-canonical-json-1"

[[loc_exceptions]]
origin_id = "{}"
owner = "policy-team"
due_phase = "{due_phase}"
remediation_record = "loc-001"
state = "{loc_state}"

[[debt_exceptions]]
origin_id = "{}"
owner = "policy-team"
due_phase = "{due_phase}"
remediation_record = "debt-001"
state = "{debt_state}"
"#,
        loc.origin_id().digest(),
        debt.origin_id().digest(),
    ))
}

pub(super) fn governance(
    origin: &OriginLedger,
    loc_state: &str,
    debt_state: &str,
    due_phase: &str,
) -> TestResult<LegacyGovernance> {
    Ok(LegacyGovernance::decode(
        governance_document(origin, loc_state, debt_state, due_phase)?.as_bytes(),
    )?)
}

pub(super) fn empty_governance() -> TestResult<LegacyGovernance> {
    Ok(LegacyGovernance::decode(
        br#"schema_version = 1
loc_exceptions = []
debt_exceptions = []

[algorithms]
analyzer = "norn-policy-1"
digest = "norn-sha256-canonical-json-1"
"#,
    )?)
}

pub(super) fn origin_current() -> TestResult<CurrentRepositoryFacts> {
    current_legacy(510, 7, true)
}

pub(super) fn missing(name: &str) -> io::Error {
    io::Error::other(format!("fixture is missing {name}"))
}
