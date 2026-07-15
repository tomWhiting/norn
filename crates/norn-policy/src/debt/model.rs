//! Closed public types for prohibited-debt analysis.

use serde::Serialize;
use thiserror::Error;

use crate::digest::{Digest, digest_bytes};
use crate::finding::ByteSpan;
use crate::path::RepositoryPath;
use crate::rust::RustSourceError;

/// Closed target kinds used in debt fingerprints.
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

impl DebtTargetKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::ProcMacro => "proc_macro",
            Self::Binary => "binary",
            Self::Example => "example",
            Self::BuildScript => "build_script",
            Self::IntegrationTest => "integration_test",
            Self::Benchmark => "benchmark",
        }
    }
}

impl From<crate::rust::cargo::CargoTargetKind> for DebtTargetKind {
    fn from(value: crate::rust::cargo::CargoTargetKind) -> Self {
        match value {
            crate::rust::cargo::CargoTargetKind::Library => Self::Library,
            crate::rust::cargo::CargoTargetKind::ProcMacro => Self::ProcMacro,
            crate::rust::cargo::CargoTargetKind::Binary => Self::Binary,
            crate::rust::cargo::CargoTargetKind::Example => Self::Example,
            crate::rust::cargo::CargoTargetKind::BuildScript => Self::BuildScript,
            crate::rust::cargo::CargoTargetKind::IntegrationTest => Self::IntegrationTest,
            crate::rust::cargo::CargoTargetKind::Benchmark => Self::Benchmark,
        }
    }
}

/// A normalized Cargo target context represented without retaining names.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DebtTargetContext {
    kind: DebtTargetKind,
    identity: Digest,
}

impl DebtTargetContext {
    /// Construct a target context from validated Cargo package and target names.
    ///
    /// The names are incorporated into a complete digest and are not retained in
    /// findings. This prevents target names from becoming free-form finding text.
    ///
    /// # Errors
    ///
    /// Returns a closed validation error for an empty, oversized, or unsupported
    /// package or target name.
    pub fn new(
        kind: DebtTargetKind,
        package: &str,
        target: &str,
    ) -> Result<Self, DebtTargetContextError> {
        validate_target_field(DebtTargetField::Package, package)?;
        validate_target_field(DebtTargetField::Target, target)?;
        let mut encoded = Vec::new();
        append_field(&mut encoded, kind.as_str().as_bytes());
        append_field(&mut encoded, package.as_bytes());
        append_field(&mut encoded, target.as_bytes());
        Ok(Self {
            kind,
            identity: digest_bytes(&encoded),
        })
    }

    /// Return the closed target kind.
    #[must_use]
    pub const fn kind(&self) -> DebtTargetKind {
        self.kind
    }

    /// Return the complete target-context identity.
    #[must_use]
    pub const fn identity(&self) -> Digest {
        self.identity
    }
}

/// Target-name fields admitted by [`DebtTargetContext`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DebtTargetField {
    /// Cargo package name.
    Package,
    /// Cargo target name.
    Target,
}

/// Invalid target context structure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DebtTargetContextError {
    /// A required target field was empty.
    #[error("debt target {field:?} is empty")]
    Empty {
        /// Invalid field.
        field: DebtTargetField,
    },
    /// A target field exceeded the fixed length bound.
    #[error("debt target {field:?} exceeds 128 bytes")]
    TooLong {
        /// Invalid field.
        field: DebtTargetField,
    },
    /// A target field contained a byte outside the closed grammar.
    #[error("debt target {field:?} contains an unsupported byte")]
    UnsupportedByte {
        /// Invalid field.
        field: DebtTargetField,
    },
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

impl DebtConstructKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::AllowAttribute => "allow_attribute",
            Self::ExpectAttribute => "expect_attribute",
            Self::IgnoreAttribute => "ignore_attribute",
            Self::ImpossibleCfg => "impossible_cfg",
            Self::UnderscoreBinding => "underscore_binding",
            Self::UnwrapCall => "unwrap_call",
            Self::UnwrapErrCall => "unwrap_err_call",
            Self::ExpectCall => "expect_call",
            Self::ExpectErrCall => "expect_err_call",
            Self::PanicMacro => "panic_macro",
            Self::TodoMacro => "todo_macro",
            Self::UnimplementedMacro => "unimplemented_macro",
            Self::UnreachableMacro => "unreachable_macro",
            Self::TodoMarker => "todo_marker",
            Self::FixmeMarker => "fixme_marker",
            Self::HackMarker => "hack_marker",
        }
    }
}

/// One stable prohibited-debt occurrence without source prose or snippets.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DebtOccurrence {
    pub(crate) path: RepositoryPath,
    pub(crate) target: DebtTargetContext,
    pub(crate) construct: DebtConstructKind,
    pub(crate) span: ByteSpan,
    pub(crate) item_identity: Digest,
    pub(crate) syntax_digest: Digest,
    pub(crate) scope_digest: Digest,
    pub(crate) ordinal: u32,
    pub(crate) fingerprint: Digest,
}

impl DebtOccurrence {
    /// Return the repository-relative source path.
    #[must_use]
    pub const fn path(&self) -> &RepositoryPath {
        &self.path
    }

    /// Return the target context.
    #[must_use]
    pub const fn target(&self) -> &DebtTargetContext {
        &self.target
    }

    /// Return the prohibited construct class.
    #[must_use]
    pub const fn construct(&self) -> DebtConstructKind {
        self.construct
    }

    /// Return the current half-open source span.
    #[must_use]
    pub const fn span(&self) -> ByteSpan {
        self.span
    }

    /// Return the normalized enclosing module/item identity digest.
    #[must_use]
    pub const fn item_identity(&self) -> Digest {
        self.item_identity
    }

    /// Return the normalized occurrence syntax/token digest.
    #[must_use]
    pub const fn syntax_digest(&self) -> Digest {
        self.syntax_digest
    }

    /// Return the structural scope digest.
    #[must_use]
    pub const fn scope_digest(&self) -> Digest {
        self.scope_digest
    }

    /// Return the zero-based ordinal in the identical-occurrence multiset.
    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Return the complete occurrence fingerprint.
    #[must_use]
    pub const fn fingerprint(&self) -> Digest {
        self.fingerprint
    }
}

/// Closed prohibited-debt scan failures.
#[derive(Debug, Error)]
pub enum DebtScanError {
    /// Rust parsing or cfg interpretation failed closed.
    #[error("Rust source could not be analyzed for prohibited debt")]
    Rust(#[from] RustSourceError),
    /// Relevant attribute metadata was malformed or unsupported.
    #[error("relevant Rust attribute is unsupported at byte {offset}")]
    Attribute {
        /// Attribute byte offset.
        offset: usize,
    },
    /// Exact cfg satisfiability analysis could not produce a trusted result.
    #[error("cfg satisfiability analysis failed closed")]
    CfgSatisfiability,
    /// A recognized Rust form lacked a required structural field.
    #[error("Rust debt syntax is unsupported at byte {offset}")]
    UnsupportedSyntax {
        /// First unsupported byte offset.
        offset: usize,
    },
    /// A source byte offset could not be represented in a finding span.
    #[error("source span cannot be represented at byte {offset}")]
    Span {
        /// First unrepresentable offset.
        offset: usize,
    },
    /// More identical occurrences existed than the versioned ordinal permits.
    #[error("identical prohibited-debt occurrence count exceeds u32")]
    Ordinal,
}

fn validate_target_field(
    field: DebtTargetField,
    value: &str,
) -> Result<(), DebtTargetContextError> {
    if value.is_empty() {
        return Err(DebtTargetContextError::Empty { field });
    }
    if value.len() > 128 {
        return Err(DebtTargetContextError::TooLong { field });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(DebtTargetContextError::UnsupportedByte { field });
    }
    Ok(())
}

fn append_field(output: &mut Vec<u8>, value: &[u8]) {
    let length = value.len().to_be_bytes();
    output.extend_from_slice(&[0_u8; 16][length.len()..]);
    output.extend_from_slice(&length);
    output.extend_from_slice(value);
}
