//! Pure Cargo workspace and target discovery over an owned snapshot.

mod discover;
mod manifest;
mod target;

use serde::Serialize;

use crate::RepositoryPath;

pub use discover::discover_cargo;

/// Whether a Cargo target participates in production analysis.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetClass {
    /// Library, proc-macro, binary, example, or build-script root.
    Production,
    /// Integration-test or benchmark root.
    TestOnly,
}

/// Closed Cargo target kinds used by reachability analysis.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetKind {
    /// Ordinary library target.
    Library,
    /// Procedural-macro library target.
    ProcMacro,
    /// Executable binary target.
    Binary,
    /// Example target, treated as production.
    Example,
    /// Package build script, treated as production.
    BuildScript,
    /// Integration-test target.
    IntegrationTest,
    /// Benchmark target.
    Benchmark,
}

/// A package root relative to the workspace authority.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "path")]
pub enum PackageRoot {
    /// The workspace root package.
    WorkspaceRoot,
    /// A member package below the workspace root.
    Member(RepositoryPath),
}

/// One discovered Cargo compilation root.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CargoTarget {
    /// Owning package name.
    package: String,
    /// Owning package root.
    package_root: PackageRoot,
    /// Cargo target kind.
    kind: CargoTargetKind,
    /// Production or test-only classification.
    class: TargetClass,
    /// Cargo target name.
    name: String,
    /// Repository-relative crate root.
    root: RepositoryPath,
}

impl CargoTarget {
    /// Borrow the owning package name.
    #[must_use]
    pub fn package(&self) -> &str {
        &self.package
    }

    /// Borrow the owning package root.
    #[must_use]
    pub const fn package_root(&self) -> &PackageRoot {
        &self.package_root
    }

    /// Return the closed target kind.
    #[must_use]
    pub const fn kind(&self) -> CargoTargetKind {
        self.kind
    }

    /// Return the production/test-only classification.
    #[must_use]
    pub const fn class(&self) -> TargetClass {
        self.class
    }

    /// Borrow the target name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Borrow the repository-relative crate root.
    #[must_use]
    pub const fn root(&self) -> &RepositoryPath {
        &self.root
    }
}

/// One local workspace package and its valid targets.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CargoPackage {
    /// Package name.
    name: String,
    /// Package root.
    root: PackageRoot,
    /// Package manifest path.
    manifest: RepositoryPath,
    /// Targets in stable kind/name/path order.
    targets: Vec<CargoTarget>,
}

impl CargoPackage {
    /// Borrow the package name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Borrow the package root.
    #[must_use]
    pub const fn root(&self) -> &PackageRoot {
        &self.root
    }

    /// Borrow the package manifest path.
    #[must_use]
    pub const fn manifest(&self) -> &RepositoryPath {
        &self.manifest
    }

    /// Borrow targets in stable kind/name/path order.
    #[must_use]
    pub fn targets(&self) -> &[CargoTarget] {
        &self.targets
    }
}

/// Closed Cargo-discovery failure categories.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoDiagnosticCode {
    /// The root `Cargo.toml` is absent.
    RootManifestMissing,
    /// A selected manifest or target is not a regular file.
    EntryNotRegular,
    /// Manifest bytes are not UTF-8.
    ManifestNotUtf8,
    /// Manifest TOML is malformed.
    ManifestMalformed,
    /// The root does not contain a valid workspace table.
    WorkspaceInvalid,
    /// A members/exclude value has the wrong shape.
    WorkspacePatternsInvalid,
    /// A workspace path glob is invalid or escapes authority.
    WorkspacePatternInvalid,
    /// A required member pattern matched no package manifest.
    MemberPatternUnmatched,
    /// A selected manifest has no valid package table/name.
    PackageInvalid,
    /// Two members declare the same package name.
    DuplicatePackageName,
    /// A package auto-discovery/build/edition option is invalid.
    PackageOptionInvalid,
    /// An explicit target table or target name is invalid.
    TargetInvalid,
    /// A target path is absolute, malformed, or escapes package authority.
    TargetPathInvalid,
    /// A required explicit or inferred target is missing.
    TargetMissing,
    /// A package declares no usable compilation target.
    NoPackageTargets,
    /// More than one standard-layout root has the same target name.
    TargetAmbiguous,
    /// Two explicit targets have the same kind and name.
    DuplicateTarget,
}

/// One stable Cargo-discovery diagnostic without source prose.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CargoDiagnostic {
    /// Closed diagnostic category.
    code: CargoDiagnosticCode,
    /// Relevant manifest or target path.
    path: RepositoryPath,
    /// Relevant target kind, when applicable.
    target_kind: Option<CargoTargetKind>,
    /// Manifest-array or workspace-pattern ordinal, when applicable.
    ordinal: Option<usize>,
}

impl CargoDiagnostic {
    /// Return the closed diagnostic category.
    #[must_use]
    pub const fn code(&self) -> CargoDiagnosticCode {
        self.code
    }

    /// Borrow the relevant manifest or target path.
    #[must_use]
    pub const fn path(&self) -> &RepositoryPath {
        &self.path
    }

    /// Return the relevant target kind, when present.
    #[must_use]
    pub const fn target_kind(&self) -> Option<CargoTargetKind> {
        self.target_kind
    }

    /// Return the manifest-array or workspace-pattern ordinal, when present.
    #[must_use]
    pub const fn ordinal(&self) -> Option<usize> {
        self.ordinal
    }
}

/// Deterministic local workspace facts and fail-closed diagnostics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CargoDiscovery {
    /// Packages in stable root/name order.
    packages: Vec<CargoPackage>,
    /// Diagnostics in stable structural order.
    diagnostics: Vec<CargoDiagnostic>,
}

impl CargoDiscovery {
    /// Return whether discovery found a nonempty package/target set without diagnostics.
    ///
    /// A valid discovery has no diagnostics, at least one package, and at
    /// least one compilation target in every package.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.diagnostics.is_empty()
            && !self.packages.is_empty()
            && self
                .packages
                .iter()
                .all(|package| !package.targets.is_empty())
    }

    /// Borrow packages in stable root/name order.
    #[must_use]
    pub fn packages(&self) -> &[CargoPackage] {
        &self.packages
    }

    /// Borrow diagnostics in stable structural order.
    #[must_use]
    pub fn diagnostics(&self) -> &[CargoDiagnostic] {
        &self.diagnostics
    }
}

pub(super) fn diagnostic(
    code: CargoDiagnosticCode,
    path: &RepositoryPath,
    target_kind: Option<CargoTargetKind>,
    ordinal: Option<usize>,
) -> CargoDiagnostic {
    CargoDiagnostic {
        code,
        path: path.clone(),
        target_kind,
        ordinal,
    }
}
