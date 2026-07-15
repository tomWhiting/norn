use std::error::Error;

use norn_policy::rust::cargo::{CargoDiscovery, discover_cargo};
use norn_policy::rust::modules::{
    FileReachability, GeneratedIncludeRegistry, ModuleAnalysis, ModuleDiagnosticCode,
};
use norn_policy::{EntryKind, OwnedSnapshot, RepositoryPath, SnapshotEntry};

pub(super) type TestResult = Result<(), Box<dyn Error>>;

pub(super) fn analyze(
    entries: &[(&str, &str)],
    registry: &GeneratedIncludeRegistry,
) -> Result<(OwnedSnapshot, CargoDiscovery, ModuleAnalysis), Box<dyn Error>> {
    let snapshot = snapshot(entries)?;
    let cargo = discover_cargo(&snapshot);
    if !cargo.is_valid() {
        return Err("test Cargo fixture is invalid".into());
    }
    let analysis = norn_policy::rust::modules::analyze_modules(&snapshot, registry);
    Ok((snapshot, cargo, analysis))
}

pub(super) fn analyze_kinds(
    entries: &[(&str, EntryKind, &str)],
) -> Result<ModuleAnalysis, Box<dyn Error>> {
    let snapshot = snapshot_kinds(entries)?;
    let cargo = discover_cargo(&snapshot);
    if !cargo.is_valid() {
        return Err("test Cargo fixture is invalid".into());
    }
    Ok(norn_policy::rust::modules::analyze_modules(
        &snapshot,
        &GeneratedIncludeRegistry::empty(),
    ))
}

pub(super) fn standard_manifest() -> &'static str {
    "[workspace]\n[package]\nname = \"app\"\nedition = \"2024\"\nbuild = false\n"
}

pub(super) fn file<'a>(analysis: &'a ModuleAnalysis, path: &str) -> Option<&'a FileReachability> {
    analysis
        .files
        .iter()
        .find(|file| file.path.as_str() == path)
}

pub(super) fn has_code(analysis: &ModuleAnalysis, code: ModuleDiagnosticCode) -> bool {
    analysis
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == code)
}

fn snapshot(entries: &[(&str, &str)]) -> Result<OwnedSnapshot, Box<dyn Error>> {
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

fn snapshot_kinds(entries: &[(&str, EntryKind, &str)]) -> Result<OwnedSnapshot, Box<dyn Error>> {
    let entries = entries
        .iter()
        .map(|(path, kind, contents)| {
            Ok((
                RepositoryPath::parse(*path)?,
                SnapshotEntry::new(*kind, contents.as_bytes().to_vec()),
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(OwnedSnapshot::try_from_entries(entries)?)
}
