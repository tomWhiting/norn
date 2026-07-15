//! Closed inputs and outputs for Rust reachability analysis.

use serde::{Deserialize, Serialize};

use crate::{Digest, RepositoryPath};

use super::super::cargo::{CargoTarget, CargoTargetKind, PackageRoot};

/// The supported generated-include registry schema.
pub const GENERATED_INCLUDE_REGISTRY_VERSION: u32 = 1;

/// A half-open byte span in one repository source.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSpan {
    /// Inclusive byte offset.
    pub start: usize,
    /// Exclusive byte offset.
    pub end: usize,
}

impl SourceSpan {
    pub(super) const fn from_offsets(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub(super) const fn is_valid(self) -> bool {
        self.start < self.end
    }
}

/// Closed target kinds stored independently from Cargo manifest syntax.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleTargetKind {
    /// Ordinary library target.
    Library,
    /// Procedural-macro library target.
    ProcMacro,
    /// Executable binary target.
    Binary,
    /// Example target.
    Example,
    /// Package build script.
    BuildScript,
    /// Integration-test target.
    IntegrationTest,
    /// Benchmark target.
    Benchmark,
}

impl From<CargoTargetKind> for ModuleTargetKind {
    fn from(value: CargoTargetKind) -> Self {
        match value {
            CargoTargetKind::Library => Self::Library,
            CargoTargetKind::ProcMacro => Self::ProcMacro,
            CargoTargetKind::Binary => Self::Binary,
            CargoTargetKind::Example => Self::Example,
            CargoTargetKind::BuildScript => Self::BuildScript,
            CargoTargetKind::IntegrationTest => Self::IntegrationTest,
            CargoTargetKind::Benchmark => Self::Benchmark,
        }
    }
}

/// Stable identity of one discovered Cargo compilation root.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleTargetIdentity {
    /// Owning package name.
    pub package: String,
    /// Member root, or `None` for the workspace-root package.
    pub package_root: Option<RepositoryPath>,
    /// Closed Cargo target kind.
    pub kind: ModuleTargetKind,
    /// Cargo target name.
    pub name: String,
    /// Repository-relative crate root.
    pub root: RepositoryPath,
}

/// Closed expected outcome declared by one proven compile-test selector.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompileTestExpectation {
    /// The selected source must fail to compile.
    CompileFail,
    /// The selected source must compile successfully.
    Pass,
}

/// One exact compile-test root admitted by a proven harness and dependency.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompileTestFixtureFact {
    /// Normalized selected Rust source path.
    pub path: RepositoryPath,
    /// Exact integration-test harness that selected the source.
    pub harness: ModuleTargetIdentity,
    /// Expected result declared by the selector method.
    pub expectation: CompileTestExpectation,
}

impl ModuleTargetIdentity {
    /// Derive the exact stable identity from a discovered Cargo target.
    #[must_use]
    pub fn from_target(target: &CargoTarget) -> Self {
        let package_root = match target.package_root() {
            PackageRoot::WorkspaceRoot => None,
            PackageRoot::Member(path) => Some(path.clone()),
        };
        Self {
            package: target.package().to_owned(),
            package_root,
            kind: target.kind().into(),
            name: target.name().to_owned(),
            root: target.root().clone(),
        }
    }
}

/// One repository input pinned by exact content digest.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HashedSourceInput {
    /// Normalized repository path.
    pub path: RepositoryPath,
    /// SHA-256 of the exact owned bytes.
    pub digest: Digest,
}

/// One admitted build-script `OUT_DIR` include invocation.
#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedIncludeRegistration {
    /// Source containing the invocation.
    pub source: RepositoryPath,
    /// Exact macro-invocation byte span.
    pub callsite: SourceSpan,
    /// Exact enclosing Rust item byte span.
    pub enclosing_item: SourceSpan,
    /// Digest of the normalized admitted invocation.
    pub invocation_digest: Digest,
    /// Cargo target through which the invocation is admitted.
    pub target: ModuleTargetIdentity,
    /// Build-script or generator source and exact digest.
    pub generator: HashedSourceInput,
    /// Complete, sorted repository input set and exact digests.
    pub inputs: Vec<HashedSourceInput>,
    /// Generated output file name without a directory component.
    pub output_basename: String,
}

/// Closed generated-include authority supplied to reachability analysis.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratedIncludeRegistry {
    /// Exact supported registry schema version.
    pub schema_version: u32,
    /// Registrations in strict structural order.
    pub entries: Vec<GeneratedIncludeRegistration>,
}

impl GeneratedIncludeRegistry {
    /// Construct an empty registry at the current schema version.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            schema_version: GENERATED_INCLUDE_REGISTRY_VERSION,
            entries: Vec::new(),
        }
    }
}

/// Aggregated reachability of one regular repository Rust source.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FileReachability {
    /// Normalized source path.
    pub path: RepositoryPath,
    /// Reachable from at least one production root or production branch.
    pub production: bool,
    /// Reachable from at least one distinct test-only root or branch.
    pub test_only: bool,
    /// Sorted Cargo targets establishing production reachability.
    pub production_targets: Vec<ModuleTargetIdentity>,
    /// Sorted Cargo targets establishing distinct test-only reachability.
    pub test_targets: Vec<ModuleTargetIdentity>,
}

/// Closed reachability failure categories.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleDiagnosticCode {
    /// Cargo discovery is not valid or internally coherent.
    CargoDiscoveryInvalid,
    /// A selected source is absent from the supplied snapshot.
    SourceMissing,
    /// A selected source is a symlink or another non-regular entry.
    EntryNotRegular,
    /// A selected Rust source is not UTF-8.
    SourceNotUtf8,
    /// The pinned Rust parser rejected the source.
    SourceParse,
    /// Conditional-compilation metadata is malformed or unsupported.
    CfgUnsupported,
    /// An attribute has unsupported structure or placement.
    AttributeUnsupported,
    /// A path attribute is not a supported string literal.
    PathNonliteral,
    /// More than one path attribute can apply in the same branch.
    PathConflict,
    /// A module declaration has no stable identifier.
    ModuleNameMissing,
    /// A module target is absent.
    ModuleMissing,
    /// Both standard module layouts exist for one declaration.
    ModuleAmbiguous,
    /// A literal module/include path escapes its package authority.
    AuthorityEscape,
    /// An include invocation is neither a literal nor registered generated form.
    IncludeUnsupported,
    /// A literal include target is absent.
    IncludeMissing,
    /// Module/include traversal re-entered a source on the active path.
    ResolutionCycle,
    /// A generated include has no exact registration.
    GeneratedIncludeUnregistered,
    /// A generated include registration or pinned source has drifted.
    GeneratedIncludeRegistryDrift,
    /// A registry entry did not identify any encountered invocation.
    GeneratedIncludeRegistryUnused,
    /// A trybuild fixture selector is dynamic, empty, or outside the supported shape.
    TrybuildSelectorUnsupported,
    /// A supported trybuild fixture selector matched no Rust source.
    TrybuildFixtureMissing,
    /// The exact snapshot does not prove a locked crates.io trybuild dependency.
    TrybuildDependencyUnverified,
    /// A compile-test source was selected more than once.
    TrybuildFixtureDuplicate,
    /// One compile-test source was selected with conflicting expectations.
    TrybuildExpectationConflict,
    /// A selected compile-test source is also production-reachable.
    TrybuildFixtureClassification,
    /// A Rust source beneath package source authority has no classification.
    UnclassifiedRustSource,
}

/// One stable reachability diagnostic without source prose or snippets.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ModuleDiagnostic {
    /// Closed diagnostic category.
    pub code: ModuleDiagnosticCode,
    /// Primary repository path.
    pub path: RepositoryPath,
    /// Relevant byte span, when one exists.
    pub span: Option<SourceSpan>,
    /// Conflicting or selected path, when one exists.
    pub related_path: Option<RepositoryPath>,
    /// Cargo target traversal that exposed the failure.
    pub target: Option<ModuleTargetIdentity>,
    /// Registry or alternative ordinal, when one exists.
    pub ordinal: Option<usize>,
}

/// Deterministic Rust reachability facts and fail-closed diagnostics.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ModuleAnalysis {
    /// Classified source files in normalized path order.
    pub files: Vec<FileReachability>,
    /// Proven compile-test roots in strict structural order.
    pub compile_test_fixtures: Vec<CompileTestFixtureFact>,
    /// Diagnostics in stable structural order.
    pub diagnostics: Vec<ModuleDiagnostic>,
}

impl ModuleAnalysis {
    /// Return whether analysis found no structural failure.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.diagnostics.is_empty()
    }
}
