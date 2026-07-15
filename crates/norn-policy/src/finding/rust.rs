//! Closed snapshot, Cargo, Rust, and debt issue types.

use serde::Serialize;

/// Unsupported snapshot entry class.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedEntryKind {
    /// A directory appeared where an ordinary file was required.
    Directory,
    /// A device, socket, or other special entry appeared.
    Special,
}

/// Closed Cargo compilation-target classes.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetKind {
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

/// Closed Cargo manifest-discovery issue.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoManifestIssue {
    /// The root `Cargo.toml` is absent.
    RootManifestMissing,
    /// A selected manifest is not a regular file.
    EntryNotRegular,
    /// Manifest bytes are not UTF-8.
    ManifestNotUtf8,
    /// Manifest TOML is malformed.
    ManifestMalformed,
    /// The root does not contain a valid workspace table.
    WorkspaceInvalid,
    /// A members or exclude value has the wrong shape.
    WorkspacePatternsInvalid,
    /// A workspace glob is invalid or escapes authority.
    WorkspacePatternInvalid,
    /// A required member pattern matched no package manifest.
    MemberPatternUnmatched,
    /// A selected manifest has no valid package table or name.
    PackageInvalid,
    /// Two members declare the same package name.
    DuplicatePackageName,
    /// A package discovery, build, or edition option is invalid.
    PackageOptionInvalid,
    /// A package declares no usable compilation target.
    NoPackageTargets,
}

/// Closed Cargo target-discovery issue.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetIssue {
    /// A selected target is not a regular file.
    EntryNotRegular,
    /// An explicit target table or target name is invalid.
    TargetInvalid,
    /// A target path is malformed or escapes package authority.
    TargetPathInvalid,
    /// A required explicit or inferred target is missing.
    TargetMissing,
    /// More than one standard-layout root has the same target name.
    TargetAmbiguous,
    /// Two explicit targets have the same kind and name.
    DuplicateTarget,
}

/// Closed Rust module-resolution issue.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleResolutionIssue {
    /// Cargo discovery is invalid or internally incoherent.
    CargoDiscoveryInvalid,
    /// A selected source is absent from the supplied snapshot.
    SourceMissing,
    /// A selected source is not a regular file.
    EntryNotRegular,
    /// A selected source is not UTF-8.
    SourceNotUtf8,
    /// The pinned Rust parser rejected the source.
    SourceParse,
    /// Conditional-compilation metadata is unsupported.
    CfgUnsupported,
    /// An attribute has unsupported structure or placement.
    AttributeUnsupported,
    /// A path attribute is not a supported string literal.
    PathNonliteral,
    /// More than one path attribute can apply in one branch.
    PathConflict,
    /// A module declaration has no stable identifier.
    ModuleNameMissing,
    /// A module target is absent.
    ModuleMissing,
    /// Both standard module layouts exist for one declaration.
    ModuleAmbiguous,
    /// A literal module or include path escapes package authority.
    AuthorityEscape,
    /// An include invocation is neither literal nor registered.
    IncludeUnsupported,
    /// A literal include target is absent.
    IncludeMissing,
    /// Module or include traversal re-entered an active source.
    ResolutionCycle,
}

/// Closed generated-include authority issue.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GeneratedIncludeIssue {
    /// A generated include has no exact registration.
    Unregistered,
    /// A registration or pinned generated source drifted.
    RegistryDrift,
    /// A registration identified no encountered invocation.
    RegistryUnused,
}

/// Prohibited production top-level form in a `mod.rs` file.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleShapeIssue {
    /// An external module declaration contains an inline body.
    InlineModule,
    /// A use declaration has no explicit visibility.
    PrivateUse,
    /// A different logic-bearing or named form appears at top level.
    OtherItem,
    /// An attribute is not attached to a permitted item.
    UnattachedAttribute,
}

/// Closed target kinds used in prohibited-debt findings.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebtTargetKind {
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

/// Closed prohibited Rust construct classes.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebtConstructKind {
    /// A lint-level `allow` attribute.
    AllowAttribute,
    /// A lint-level `expect` attribute.
    ExpectAttribute,
    /// An ignored test attribute.
    IgnoreAttribute,
    /// A cfg predicate with no satisfying assignment.
    ImpossibleCfg,
    /// A named binding beginning with an underscore.
    UnderscoreBinding,
    /// A method call extracting an infallible result value.
    UnwrapCall,
    /// A method call extracting an infallible error value.
    UnwrapErrCall,
    /// A message-bearing method call extracting a result value.
    ExpectCall,
    /// A message-bearing method call extracting an error value.
    ExpectErrCall,
    /// A panic macro invocation.
    PanicMacro,
    /// An unfinished-work macro invocation.
    TodoMacro,
    /// An unimplemented-work macro invocation.
    UnimplementedMacro,
    /// An unreachable-code macro invocation.
    UnreachableMacro,
    /// An unresolved task marker.
    TodoMarker,
    /// An unresolved repair marker.
    FixmeMarker,
    /// An unresolved shortcut marker.
    HackMarker,
}
