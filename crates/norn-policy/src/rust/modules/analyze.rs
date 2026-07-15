//! Cargo-root orchestration and complete source classification.

use std::collections::{BTreeMap, BTreeSet};

use super::super::cargo::{
    CargoDiscovery, CargoPackage, CargoTargetKind, PackageRoot, TargetClass, discover_cargo,
};
use super::generated::GeneratedAuthority;
use super::model::{
    CompileTestFixtureFact, FileReachability, GeneratedIncludeRegistry, ModuleAnalysis,
    ModuleDiagnostic, ModuleDiagnosticCode, ModuleTargetIdentity,
};
use super::path::{Directory, is_beneath, package_authority};
use crate::{EntryKind, OwnedSnapshot, RepositoryPath};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct ReachModes {
    pub(super) production: bool,
    pub(super) test: bool,
    pub(super) fixture: bool,
}

impl ReachModes {
    pub(super) const fn production_root() -> Self {
        Self {
            production: true,
            test: true,
            fixture: false,
        }
    }

    pub(super) const fn test_root() -> Self {
        Self {
            production: false,
            test: true,
            fixture: false,
        }
    }

    pub(super) const fn fixture_root() -> Self {
        Self {
            production: false,
            test: false,
            fixture: true,
        }
    }

    pub(super) const fn any(self) -> bool {
        self.production || self.test || self.fixture
    }

    pub(super) fn merge(&mut self, other: Self) {
        self.production |= other.production;
        self.test |= other.test;
        self.fixture |= other.fixture;
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct VisitKey {
    pub(super) path: RepositoryPath,
    pub(super) module_directory: Directory,
    pub(super) target: ModuleTargetIdentity,
    pub(super) modes: ReachModes,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ReachFacts {
    production_targets: BTreeSet<ModuleTargetIdentity>,
    test_targets: BTreeSet<ModuleTargetIdentity>,
}

pub(super) struct Analyzer<'snapshot, 'registry> {
    pub(super) snapshot: &'snapshot OwnedSnapshot,
    pub(super) diagnostics: Vec<ModuleDiagnostic>,
    pub(super) generated: GeneratedAuthority<'registry>,
    pub(super) visited: BTreeSet<VisitKey>,
    pub(super) stack: Vec<RepositoryPath>,
    pub(super) compile_test_fixtures: BTreeSet<CompileTestFixtureFact>,
    reachability: BTreeMap<RepositoryPath, ReachFacts>,
    reached_packages: BTreeMap<RepositoryPath, BTreeSet<Option<RepositoryPath>>>,
}

/// Compute deterministic Rust file reachability without filesystem access.
#[must_use]
pub fn analyze_modules(
    snapshot: &OwnedSnapshot,
    registry: &GeneratedIncludeRegistry,
) -> ModuleAnalysis {
    analyze_modules_with_cargo(snapshot, registry).1
}

/// Derive Cargo once and return the exact authority consumed by reachability.
pub(crate) fn analyze_modules_with_cargo(
    snapshot: &OwnedSnapshot,
    registry: &GeneratedIncludeRegistry,
) -> (CargoDiscovery, ModuleAnalysis) {
    let cargo = discover_cargo(snapshot);
    let analysis = analyze_derived_modules(snapshot, &cargo, registry);
    (cargo, analysis)
}

fn analyze_derived_modules(
    snapshot: &OwnedSnapshot,
    cargo: &CargoDiscovery,
    registry: &GeneratedIncludeRegistry,
) -> ModuleAnalysis {
    if let Some(path) = invalid_cargo_path(snapshot, cargo) {
        return ModuleAnalysis {
            files: Vec::new(),
            compile_test_fixtures: Vec::new(),
            diagnostics: vec![ModuleDiagnostic {
                code: ModuleDiagnosticCode::CargoDiscoveryInvalid,
                path,
                span: None,
                related_path: None,
                target: None,
                ordinal: None,
            }],
        };
    }
    let mut diagnostics = Vec::new();
    let generated = GeneratedAuthority::new(snapshot, cargo, registry, &mut diagnostics);
    let mut analyzer = Analyzer {
        snapshot,
        diagnostics,
        generated,
        visited: BTreeSet::new(),
        stack: Vec::new(),
        compile_test_fixtures: BTreeSet::new(),
        reachability: BTreeMap::new(),
        reached_packages: BTreeMap::new(),
    };
    for package in cargo.packages() {
        for target in package.targets() {
            let identity = ModuleTargetIdentity::from_target(target);
            let modes = match target.class() {
                TargetClass::Production => ReachModes::production_root(),
                TargetClass::TestOnly => ReachModes::test_root(),
            };
            analyzer.visit_file(
                package_root(package.root()),
                &identity,
                target.root(),
                &Directory::parent_of(target.root()),
                modes,
            );
        }
    }
    analyzer.discover_trybuild_fixtures(cargo.packages());
    analyzer.generated.finish(&mut analyzer.diagnostics);
    analyzer.classify_unreferenced(cargo.packages());
    analyzer.finish()
}

impl Analyzer<'_, '_> {
    pub(super) fn is_exclusive_test_reachable(
        &self,
        path: &RepositoryPath,
        target: &ModuleTargetIdentity,
    ) -> bool {
        self.reachability.get(path).is_some_and(|facts| {
            facts.production_targets.is_empty() && facts.test_targets.contains(target)
        })
    }

    pub(super) fn record(
        &mut self,
        package_root: Option<&RepositoryPath>,
        target: &ModuleTargetIdentity,
        path: &RepositoryPath,
        modes: ReachModes,
    ) {
        let facts = self.reachability.entry(path.clone()).or_default();
        if modes.production {
            facts.production_targets.insert(target.clone());
        }
        if (modes.test || modes.fixture) && !modes.production {
            facts.test_targets.insert(target.clone());
        }
        self.reached_packages
            .entry(path.clone())
            .or_default()
            .insert(package_root.cloned());
    }

    pub(super) fn problem(
        &mut self,
        code: ModuleDiagnosticCode,
        path: &RepositoryPath,
        span: Option<super::model::SourceSpan>,
        related_path: Option<RepositoryPath>,
        target: Option<ModuleTargetIdentity>,
        ordinal: Option<usize>,
    ) {
        self.diagnostics.push(ModuleDiagnostic {
            code,
            path: path.clone(),
            span,
            related_path,
            target,
            ordinal,
        });
    }

    fn classify_unreferenced(&mut self, packages: &[CargoPackage]) {
        for (path, entry) in self.snapshot.iter() {
            if !path.file_name().as_bytes().ends_with(b".rs") {
                continue;
            }
            let Some(package) = owning_package(path, packages) else {
                continue;
            };
            let root = package_root(package.root());
            let reached = self.reached_packages.get(path);
            let classified = reached.is_some_and(|roots| roots.contains(&root.cloned()));
            let crossed_package = reached.is_some_and(|roots| {
                roots
                    .iter()
                    .any(|reached_root| reached_root.as_ref() != root)
            });
            if entry.kind() != EntryKind::Regular {
                self.problem(
                    ModuleDiagnosticCode::EntryNotRegular,
                    path,
                    None,
                    None,
                    None,
                    None,
                );
            } else if !classified || crossed_package {
                self.problem(
                    ModuleDiagnosticCode::UnclassifiedRustSource,
                    path,
                    None,
                    None,
                    None,
                    None,
                );
            }
        }
    }

    fn finish(mut self) -> ModuleAnalysis {
        let files = self
            .reachability
            .into_iter()
            .map(|(path, facts)| FileReachability {
                path,
                production: !facts.production_targets.is_empty(),
                test_only: !facts.test_targets.is_empty(),
                production_targets: facts.production_targets.into_iter().collect(),
                test_targets: facts.test_targets.into_iter().collect(),
            })
            .collect();
        self.diagnostics.sort();
        self.diagnostics.dedup();
        ModuleAnalysis {
            files,
            compile_test_fixtures: self.compile_test_fixtures.into_iter().collect(),
            diagnostics: self.diagnostics,
        }
    }
}

fn invalid_cargo_path(snapshot: &OwnedSnapshot, cargo: &CargoDiscovery) -> Option<RepositoryPath> {
    if let Some(diagnostic) = cargo.diagnostics().first() {
        return Some(diagnostic.path().clone());
    }
    if cargo
        .packages()
        .windows(2)
        .any(|pair| !package_precedes(&pair[0], &pair[1]))
    {
        return cargo
            .packages()
            .first()
            .map(|package| package.manifest().clone());
    }
    let mut targets = BTreeSet::new();
    for package in cargo.packages() {
        let expected_manifest = package_manifest(package.root());
        let manifest_is_regular = snapshot
            .get(package.manifest())
            .is_some_and(|entry| entry.kind() == EntryKind::Regular);
        if expected_manifest.as_ref() != Some(package.manifest())
            || !manifest_is_regular
            || package.targets().is_empty()
            || package.targets().windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Some(package.manifest().clone());
        }
        let authority = package_authority(package_root(package.root()));
        for target in package.targets() {
            if target.package() != package.name()
                || target.package_root() != package.root()
                || target.class() != expected_class(target.kind())
                || !is_beneath(target.root(), &authority)
                || snapshot
                    .get(target.root())
                    .is_none_or(|entry| entry.kind() != EntryKind::Regular)
                || !targets.insert(ModuleTargetIdentity::from_target(target))
            {
                return Some(package.manifest().clone());
            }
        }
    }
    None
}

fn package_manifest(root: &PackageRoot) -> Option<RepositoryPath> {
    let raw = match root {
        PackageRoot::WorkspaceRoot => "Cargo.toml".to_owned(),
        PackageRoot::Member(path) => format!("{path}/Cargo.toml"),
    };
    let Ok(path) = RepositoryPath::parse(raw) else {
        return None;
    };
    Some(path)
}

const fn expected_class(kind: CargoTargetKind) -> TargetClass {
    match kind {
        CargoTargetKind::Library
        | CargoTargetKind::ProcMacro
        | CargoTargetKind::Binary
        | CargoTargetKind::Example
        | CargoTargetKind::BuildScript => TargetClass::Production,
        CargoTargetKind::IntegrationTest | CargoTargetKind::Benchmark => TargetClass::TestOnly,
    }
}

fn package_precedes(left: &CargoPackage, right: &CargoPackage) -> bool {
    left.root()
        .cmp(right.root())
        .then_with(|| left.name().cmp(right.name()))
        .then_with(|| left.manifest().cmp(right.manifest()))
        .then_with(|| left.targets().cmp(right.targets()))
        .is_lt()
}

pub(super) fn owning_package<'a>(
    path: &RepositoryPath,
    packages: &'a [CargoPackage],
) -> Option<&'a CargoPackage> {
    packages
        .iter()
        .filter(|package| source_authority_contains(path, package))
        .max_by_key(|package| package_root_depth(package.root()))
}

fn source_authority_contains(path: &RepositoryPath, package: &CargoPackage) -> bool {
    if package.targets().iter().any(|target| target.root() == path) {
        return true;
    }
    let root = package_root(package.root());
    let relative = relative_to_root(path, root);
    let Some(relative) = relative else {
        return false;
    };
    if ["src/", "examples/", "tests/", "benches/"]
        .iter()
        .any(|prefix| relative.starts_with(prefix))
    {
        return true;
    }
    package.targets().iter().any(|target| {
        target.root().parent().is_some_and(|parent| {
            root != Some(&parent)
                && (path == &parent || path.as_str().starts_with(&format!("{parent}/")))
        })
    })
}

fn relative_to_root<'a>(
    path: &'a RepositoryPath,
    root: Option<&RepositoryPath>,
) -> Option<&'a str> {
    match root {
        None => Some(path.as_str()),
        Some(root) if path == root => Some(""),
        Some(root) => path.as_str().strip_prefix(&format!("{root}/")),
    }
}

pub(super) fn package_root(root: &PackageRoot) -> Option<&RepositoryPath> {
    match root {
        PackageRoot::WorkspaceRoot => None,
        PackageRoot::Member(path) => Some(path),
    }
}

fn package_root_depth(root: &PackageRoot) -> usize {
    package_root(root).map_or(0, |path| path.as_str().split('/').count())
}
