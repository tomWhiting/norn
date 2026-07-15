//! Canonical repository fact-graph integration tests.

use std::error::Error;

use norn_policy::baseline::{BaselineFactsError, RepositoryBaselineFacts};
use norn_policy::debt::DebtConstructKind;
use norn_policy::facts::{
    FactFailureCode, RepositoryFactsError, SourceInventoryEntry, analyze_facts,
    source_inventory_identity,
};
use norn_policy::rust::modules::{GeneratedIncludeRegistry, ModuleTargetKind};
use norn_policy::{OwnedSnapshot, RepositoryPath, SnapshotEntry};

type TestResult = Result<(), Box<dyn Error>>;

const MANIFEST: &str =
    "[workspace]\n[package]\nname = \"app\"\nedition = \"2024\"\nbuild = false\n";

const HELPER: &str = "pub fn value() -> u8 { 7 }\n";

#[test]
fn facts_compose_every_production_analyzer_deterministically() -> TestResult {
    let library = library_source();
    let forward = fixture(&[
        ("Cargo.toml", MANIFEST),
        ("src/lib.rs", &library),
        ("src/helper.rs", HELPER),
    ])?;
    let reverse = fixture(&[
        ("src/helper.rs", HELPER),
        ("src/lib.rs", &library),
        ("Cargo.toml", MANIFEST),
    ])?;
    let generated = GeneratedIncludeRegistry::empty();

    let left = analyze_facts(&forward, &generated);
    let right = analyze_facts(&reverse, &generated);

    assert_eq!(left, right);
    assert!(left.is_structurally_valid());
    assert!(left.failures().is_empty());
    assert_eq!(
        left.source_inventory()
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        ["src/helper.rs", "src/lib.rs"]
    );
    assert_eq!(
        left.production_files()
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["src/helper.rs", "src/lib.rs"]
    );
    assert!(left.production_files().iter().all(|file| {
        file.metrics.loc > 0
            && file.targets.len() == 1
            && file.targets[0].kind == ModuleTargetKind::Library
    }));
    assert!(production_file(&left, "src/lib.rs")?.is_entrypoint());
    assert!(!production_file(&left, "src/helper.rs")?.is_entrypoint());
    assert_eq!(left.debt().len(), 1);
    assert_eq!(left.debt()[0].construct(), DebtConstructKind::PanicMacro);

    let writers = left
        .writers()
        .ok_or_else(|| std::io::Error::other("writer inventory was not constructed"))?;
    assert_eq!(writers.operations().len(), 1);
    assert!(
        writers
            .operations()
            .iter()
            .all(|operation| operation.path().as_str() == "src/lib.rs")
    );
    assert_eq!(
        writers
            .sources()
            .iter()
            .map(|source| (source.path().as_str(), source.content()))
            .collect::<Vec<_>>(),
        left.source_inventory()
            .iter()
            .filter(|source| source.production)
            .map(|source| (source.path.as_str(), source.content))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        writers.registry_digest(),
        norn_policy::writers::builtin_sink_registry()?.digest()
    );
    Ok(())
}

#[test]
fn facts_retain_test_only_items_without_treating_them_as_production() -> TestResult {
    let library = library_source();
    let snapshot = fixture(&[
        ("Cargo.toml", MANIFEST),
        ("src/lib.rs", &library),
        ("src/helper.rs", HELPER),
    ])?;
    let facts = analyze_facts(&snapshot, &GeneratedIncludeRegistry::empty());

    let test_only = facts
        .items()
        .iter()
        .filter(|fact| fact.item.test_only_count() > 0)
        .collect::<Vec<_>>();
    assert_eq!(test_only.len(), 2);
    assert!(
        test_only
            .iter()
            .all(|fact| fact.path.as_str() == "src/lib.rs")
    );
    assert!(
        facts
            .items()
            .iter()
            .any(|fact| fact.item.production_count() > 0)
    );
    assert_eq!(facts.debt().len(), 1);
    assert_eq!(facts.debt()[0].construct(), DebtConstructKind::PanicMacro);
    Ok(())
}

#[test]
fn test_only_cargo_root_forces_all_items_test_only_and_retains_debt() -> TestResult {
    let integration = format!(
        "#[test]\nfn integration() {{ {}(\"test debt\"); }}\n",
        prohibited_macro_name()
    );
    let snapshot = fixture(&[
        ("Cargo.toml", MANIFEST),
        ("src/lib.rs", "pub fn value() -> u8 { 7 }\n"),
        ("tests/integration.rs", &integration),
    ])?;
    let facts = analyze_facts(&snapshot, &GeneratedIncludeRegistry::empty());

    assert!(facts.is_structurally_valid());
    assert_eq!(facts.debt().len(), 1);
    assert_eq!(facts.debt()[0].path().as_str(), "tests/integration.rs");
    assert_eq!(facts.debt()[0].construct(), DebtConstructKind::PanicMacro);
    assert!(
        facts
            .source_inventory()
            .iter()
            .any(|entry| entry.path.as_str() == "tests/integration.rs" && entry.test_only)
    );
    assert!(
        facts
            .production_files()
            .iter()
            .all(|file| file.path.as_str() != "tests/integration.rs")
    );
    let integration_items = facts
        .items()
        .iter()
        .filter(|item| item.path.as_str() == "tests/integration.rs")
        .collect::<Vec<_>>();
    assert!(!integration_items.is_empty());
    assert!(integration_items.iter().all(|item| {
        item.item.production_count() == 0
            && item.item.test_only_count() > 0
            && item.item.production_spans().is_empty()
            && u32::try_from(item.item.test_only_spans().len()) == Ok(item.item.test_only_count())
    }));
    Ok(())
}

#[test]
fn entrypoint_classification_uses_cargo_roots_instead_of_basenames() -> TestResult {
    let manifest = r#"
[workspace]
[package]
name = "app"
edition = "2024"
build = false
autolib = false
autobins = false

[lib]
path = "src/api.rs"
"#;
    let snapshot = fixture(&[
        ("Cargo.toml", manifest),
        (
            "src/api.rs",
            "mod main;\npub fn value() -> u8 { main::value() }\n",
        ),
        ("src/main.rs", "pub fn value() -> u8 { 7 }\n"),
    ])?;
    let facts = analyze_facts(&snapshot, &GeneratedIncludeRegistry::empty());

    let api = production_file(&facts, "src/api.rs")?;
    let nested_main = production_file(&facts, "src/main.rs")?;
    assert!(api.is_entrypoint());
    assert!(!nested_main.is_entrypoint());
    Ok(())
}

#[test]
fn source_inventory_digest_covers_exact_classified_bytes() -> TestResult {
    let library = library_source();
    let original = fixture(&[
        ("Cargo.toml", MANIFEST),
        ("src/lib.rs", &library),
        ("src/helper.rs", HELPER),
    ])?;
    let changed = fixture(&[
        ("Cargo.toml", MANIFEST),
        ("src/lib.rs", &library),
        ("src/helper.rs", "pub fn value() -> u8 { 8 }\n"),
    ])?;
    let generated = GeneratedIncludeRegistry::empty();

    let original = analyze_facts(&original, &generated);
    let changed = analyze_facts(&changed, &generated);

    assert_ne!(
        original.source_inventory_digest(),
        changed.source_inventory_digest()
    );
    assert_ne!(original.source_inventory(), changed.source_inventory());
    Ok(())
}

#[test]
fn source_inventory_rows_round_trip_and_identity_binds_order_and_classification() -> TestResult {
    let rows = vec![
        SourceInventoryEntry {
            path: RepositoryPath::parse("src/lib.rs")?,
            content: norn_policy::digest_bytes(b"lib"),
            production: true,
            test_only: false,
        },
        SourceInventoryEntry {
            path: RepositoryPath::parse("tests/ui.rs")?,
            content: norn_policy::digest_bytes(b"ui"),
            production: false,
            test_only: true,
        },
    ];
    let encoded = serde_json::to_vec(&rows)?;
    let decoded: Vec<SourceInventoryEntry> = serde_json::from_slice(&encoded)?;
    assert_eq!(decoded, rows);
    assert_eq!(
        source_inventory_identity(&decoded),
        source_inventory_identity(&rows)
    );

    let mut reversed = rows.clone();
    reversed.reverse();
    assert_ne!(
        source_inventory_identity(&reversed),
        source_inventory_identity(&rows)
    );
    let mut reclassified = rows.clone();
    reclassified[1].production = true;
    assert_ne!(
        source_inventory_identity(&reclassified),
        source_inventory_identity(&rows)
    );
    Ok(())
}

#[test]
fn structurally_invalid_reachability_never_looks_ready() -> TestResult {
    let snapshot = fixture(&[("Cargo.toml", MANIFEST), ("src/lib.rs", "mod missing;\n")])?;
    let facts = analyze_facts(&snapshot, &GeneratedIncludeRegistry::empty());

    assert!(!facts.modules().is_valid());
    assert!(!facts.is_structurally_valid());
    assert_eq!(facts.production_files().len(), 1);
    assert_eq!(facts.source_inventory().len(), 1);
    assert!(!facts.failures().iter().any(|failure| {
        matches!(
            failure.code,
            FactFailureCode::ProductionMetrics
                | FactFailureCode::DebtAnalysis
                | FactFailureCode::WriterAnalysis
        )
    }));
    assert!(matches!(
        RepositoryBaselineFacts::try_from_repository(&facts),
        Err(BaselineFactsError::Repository(
            RepositoryFactsError::Modules
        ))
    ));
    Ok(())
}

fn fixture(entries: &[(&str, &str)]) -> Result<OwnedSnapshot, Box<dyn Error>> {
    let entries = entries
        .iter()
        .map(|(path, contents)| {
            Ok((
                RepositoryPath::parse(*path)?,
                SnapshotEntry::regular(contents.as_bytes().to_vec()),
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}

fn production_file<'a>(
    facts: &'a norn_policy::facts::RepositoryFacts,
    path: &str,
) -> Result<&'a norn_policy::facts::ProductionFileFact, std::io::Error> {
    facts
        .production_files()
        .iter()
        .find(|file| file.path.as_str() == path)
        .ok_or_else(|| std::io::Error::other("production fact is absent"))
}

fn library_source() -> String {
    format!(
        r#"
mod helper;

pub fn save() {{
    let result = std::fs::write("artifact", b"data");
    drop(result);
}}

#[cfg(test)]
fn test_only_writer_and_debt() {{
    let result = std::fs::write("test-artifact", b"data");
    drop(result);
    {}("test-only sentinel");
}}
"#,
        prohibited_macro_name()
    )
}

fn prohibited_macro_name() -> String {
    ["pan", "ic!"].concat()
}
