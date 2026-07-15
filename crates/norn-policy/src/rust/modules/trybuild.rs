//! Explicit trybuild fixture discovery from active integration-test roots.

mod dependency;
mod scan;

use std::collections::BTreeMap;

use super::super::RustSource;
use super::super::cargo::{CargoPackage, CargoTargetKind};
use super::analyze::{Analyzer, ReachModes, owning_package, package_root};
use super::model::{
    CompileTestExpectation, CompileTestFixtureFact, ModuleDiagnosticCode, ModuleTargetIdentity,
    SourceSpan,
};
use super::path::Directory;
use crate::{EntryKind, RepositoryPath};
use dependency::is_verified;
use scan::{SelectorArgument, SelectorObservation, scan_selectors};

enum SelectorShape {
    Exact(RepositoryPath),
    Glob(RepositoryPath),
}

enum SelectorFailure {
    Escape,
    Unsupported,
}

#[derive(Clone)]
struct SelectedFixture {
    span: SourceSpan,
    expectation: CompileTestExpectation,
}

impl Analyzer<'_, '_> {
    pub(super) fn discover_trybuild_fixtures(&mut self, packages: &[CargoPackage]) {
        for package in packages {
            for target in package
                .targets()
                .iter()
                .filter(|target| target.kind() == CargoTargetKind::IntegrationTest)
            {
                let Some(entry) = self.snapshot.get(target.root()) else {
                    continue;
                };
                let Ok(source) = RustSource::parse(entry.bytes().to_vec()) else {
                    continue;
                };
                let identity = ModuleTargetIdentity::from_target(target);
                let observations = scan_selectors(&source);
                let Some(first) = observations.first() else {
                    continue;
                };
                if !is_verified(self.snapshot, package, target) {
                    self.selector_problem(
                        ModuleDiagnosticCode::TrybuildDependencyUnverified,
                        &identity,
                        first.span,
                        None,
                    );
                    continue;
                }
                self.apply_harness(package, packages, &identity, observations);
            }
        }
    }

    fn apply_harness(
        &mut self,
        package: &CargoPackage,
        packages: &[CargoPackage],
        target: &ModuleTargetIdentity,
        observations: Vec<SelectorObservation>,
    ) {
        let mut selected = BTreeMap::new();
        let mut valid = true;
        for observation in observations {
            if !self.collect_selector(package, target, observation, &mut selected) {
                valid = false;
            }
        }
        if !valid || !self.validate_selection(package, packages, target, &selected) {
            return;
        }
        let diagnostic_count = self.diagnostics.len();
        for path in selected.keys() {
            self.visit_file(
                package_root(package.root()),
                target,
                &path,
                &Directory::parent_of(&path),
                ReachModes::fixture_root(),
            );
        }
        if self.diagnostics.len() != diagnostic_count
            || !self.validate_fixture_classification(target, &selected)
        {
            return;
        }
        for (path, selected) in selected {
            self.compile_test_fixtures.insert(CompileTestFixtureFact {
                path,
                harness: target.clone(),
                expectation: selected.expectation,
            });
        }
    }

    fn collect_selector(
        &mut self,
        package: &CargoPackage,
        target: &ModuleTargetIdentity,
        observation: SelectorObservation,
        selected: &mut BTreeMap<RepositoryPath, SelectedFixture>,
    ) -> bool {
        let SelectorArgument::Literal(raw) = observation.argument else {
            self.selector_problem(
                ModuleDiagnosticCode::TrybuildSelectorUnsupported,
                target,
                observation.span,
                None,
            );
            return false;
        };
        let shape = match SelectorShape::parse(package, &raw) {
            Ok(shape) => shape,
            Err(SelectorFailure::Escape) => {
                self.selector_problem(
                    ModuleDiagnosticCode::AuthorityEscape,
                    target,
                    observation.span,
                    None,
                );
                return false;
            }
            Err(SelectorFailure::Unsupported) => {
                self.selector_problem(
                    ModuleDiagnosticCode::TrybuildSelectorUnsupported,
                    target,
                    observation.span,
                    None,
                );
                return false;
            }
        };
        let expectation = observation.expectation;
        let paths = self.select_paths(&shape);
        if paths.is_empty() {
            self.selector_problem(
                ModuleDiagnosticCode::TrybuildFixtureMissing,
                target,
                observation.span,
                Some(shape.path().clone()),
            );
            return false;
        }
        for path in paths {
            if let Some(previous) = selected.get(&path) {
                let code = if previous.expectation == expectation {
                    ModuleDiagnosticCode::TrybuildFixtureDuplicate
                } else {
                    ModuleDiagnosticCode::TrybuildExpectationConflict
                };
                self.selector_problem(code, target, observation.span, Some(path));
                return false;
            }
            selected.insert(
                path,
                SelectedFixture {
                    span: observation.span,
                    expectation,
                },
            );
        }
        true
    }

    fn select_paths(&self, shape: &SelectorShape) -> Vec<RepositoryPath> {
        match shape {
            SelectorShape::Exact(path) if self.snapshot.contains_path(path) => vec![path.clone()],
            SelectorShape::Exact(_) => Vec::new(),
            SelectorShape::Glob(directory) => {
                let prefix = format!("{directory}/");
                self.snapshot
                    .iter()
                    .filter_map(|(path, _)| {
                        let relative = path.as_str().strip_prefix(&prefix)?;
                        if relative.contains('/') || relative == ".rs" || !relative.ends_with(".rs")
                        {
                            None
                        } else {
                            Some(path.clone())
                        }
                    })
                    .collect()
            }
        }
    }

    fn validate_selection(
        &mut self,
        package: &CargoPackage,
        packages: &[CargoPackage],
        target: &ModuleTargetIdentity,
        selected: &BTreeMap<RepositoryPath, SelectedFixture>,
    ) -> bool {
        let mut valid = true;
        for (path, selection) in selected {
            let owned =
                owning_package(path, packages).is_some_and(|owner| owner.root() == package.root());
            if !owned {
                self.selector_problem(
                    ModuleDiagnosticCode::AuthorityEscape,
                    target,
                    selection.span,
                    Some(path.clone()),
                );
                valid = false;
            }
            if self
                .snapshot
                .get(path)
                .is_none_or(|entry| entry.kind() != EntryKind::Regular)
            {
                self.problem(
                    ModuleDiagnosticCode::EntryNotRegular,
                    path,
                    None,
                    None,
                    Some(target.clone()),
                    None,
                );
                valid = false;
            }
        }
        valid
    }

    fn validate_fixture_classification(
        &mut self,
        target: &ModuleTargetIdentity,
        selected: &BTreeMap<RepositoryPath, SelectedFixture>,
    ) -> bool {
        let mut valid = true;
        for (path, selection) in selected {
            let exclusive_test = self.is_exclusive_test_reachable(path, target);
            let duplicate = self
                .compile_test_fixtures
                .iter()
                .find(|fact| fact.path == *path);
            if let Some(previous) = duplicate {
                let code = if previous.expectation == selection.expectation {
                    ModuleDiagnosticCode::TrybuildFixtureDuplicate
                } else {
                    ModuleDiagnosticCode::TrybuildExpectationConflict
                };
                self.selector_problem(code, target, selection.span, Some(path.clone()));
                valid = false;
            } else if !exclusive_test {
                self.selector_problem(
                    ModuleDiagnosticCode::TrybuildFixtureClassification,
                    target,
                    selection.span,
                    Some(path.clone()),
                );
                valid = false;
            }
        }
        valid
    }

    fn selector_problem(
        &mut self,
        code: ModuleDiagnosticCode,
        target: &ModuleTargetIdentity,
        selector_span: SourceSpan,
        related_path: Option<RepositoryPath>,
    ) {
        self.problem(
            code,
            &target.root,
            Some(selector_span),
            related_path,
            Some(target.clone()),
            None,
        );
    }
}

impl SelectorShape {
    fn parse(package: &CargoPackage, raw: &str) -> Result<Self, SelectorFailure> {
        if raw.is_empty() {
            return Err(SelectorFailure::Unsupported);
        }
        if path_escapes(raw) {
            return Err(SelectorFailure::Escape);
        }
        let wildcard = raw.strip_suffix("/*.rs");
        let relative = match wildcard {
            Some(directory) => directory,
            None => raw,
        };
        if has_pattern_meta(relative) {
            return Err(SelectorFailure::Unsupported);
        }
        let components = relative.split('/').collect::<Vec<_>>();
        let dedicated_subtree = components.first().copied() == Some("tests")
            && components.get(1).is_some_and(|value| !value.is_empty());
        if !dedicated_subtree {
            return Err(SelectorFailure::Unsupported);
        }
        if wildcard.is_none()
            && (components.len() < 3
                || !relative.ends_with(".rs")
                || components.last().copied() == Some(".rs"))
        {
            return Err(SelectorFailure::Unsupported);
        }
        let path = rooted_path(package, relative).ok_or(SelectorFailure::Escape)?;
        Ok(if wildcard.is_some() {
            Self::Glob(path)
        } else {
            Self::Exact(path)
        })
    }

    const fn path(&self) -> &RepositoryPath {
        match self {
            Self::Exact(path) | Self::Glob(path) => path,
        }
    }
}

fn rooted_path(package: &CargoPackage, relative: &str) -> Option<RepositoryPath> {
    let raw = package_root(package.root())
        .map_or_else(|| relative.to_owned(), |root| format!("{root}/{relative}"));
    let Ok(path) = RepositoryPath::parse(raw) else {
        return None;
    };
    Some(path)
}

fn path_escapes(raw: &str) -> bool {
    raw.starts_with('/')
        || raw.contains(['\\', '\0'])
        || raw.chars().any(char::is_control)
        || raw
            .split('/')
            .any(|component| matches!(component, "" | "." | ".."))
}

fn has_pattern_meta(raw: &str) -> bool {
    raw.contains(['*', '?', '[', ']', '{', '}'])
}
