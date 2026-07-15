//! Closed writer-analysis data types.

mod inventory;
mod unknown;

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::digest::Digest;
use crate::finding::ByteSpan;
use crate::path::RepositoryPath;

pub(crate) use inventory::writer_source_inventory_digest;
pub use inventory::{WriterInventory, WriterSourceIdentity};
pub use unknown::UnknownSinkReason;

/// Schema version for writer registries and generated inventories.
pub const WRITER_SCHEMA_VERSION: u32 = 1;

/// Analyzer identity included in writer operation fingerprints.
pub const WRITER_ANALYZER_VERSION: &str = "norn-writers-4";

/// A validated machine-facing writer identifier.
///
/// Tokens contain lowercase ASCII letters, digits, `.`, `_`, `:`, and `-`.
/// They cannot carry source snippets or rendered messages.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WriterToken(String);

impl WriterToken {
    /// Parse a token using the closed writer-token grammar.
    ///
    /// # Errors
    ///
    /// Returns the precise structural error for an invalid token.
    pub fn parse(value: impl Into<String>) -> Result<Self, WriterTokenError> {
        let value = value.into();
        if value.is_empty() {
            return Err(WriterTokenError::Empty);
        }
        if !value.bytes().all(is_token_byte) {
            return Err(WriterTokenError::UnsupportedByte);
        }
        Ok(Self(value))
    }

    pub(crate) fn from_static(value: &'static str) -> Self {
        Self(value.to_owned())
    }

    pub(crate) fn from_trusted(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the validated token.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WriterToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("WriterToken").field(&self.0).finish()
    }
}

impl fmt::Display for WriterToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for WriterToken {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for WriterToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

/// Invalid writer-token structure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WriterTokenError {
    /// No token bytes were supplied.
    #[error("writer token is empty")]
    Empty,
    /// A byte is outside the closed grammar.
    #[error("writer token contains an unsupported byte")]
    UnsupportedByte,
}

const fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b':' | b'-')
}

/// Authority or handle state propagated through local expressions.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowClass {
    /// The expression does not yield writer authority.
    None,
    /// A pinned project-private root or equivalent authority.
    RootAuthority,
    /// A writable file or stream handle.
    WritableHandle,
    /// A temporary-file handle supporting persistence operations.
    TemporaryHandle,
    /// A standard-library builder controlling a later writer open.
    StandardOpenBuilder,
    /// A Tokio builder controlling a later writer open.
    TokioOpenBuilder,
    /// A tempfile builder controlling a later writer open.
    TempfileBuilder,
    /// Preserve the receiver's flow class.
    SameReceiver,
    /// Preserve the first argument's flow class.
    FirstArgument,
}

impl FlowClass {
    pub(crate) const fn token(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::RootAuthority => "root_authority",
            Self::WritableHandle => "writable_handle",
            Self::TemporaryHandle => "temporary_handle",
            Self::StandardOpenBuilder => "standard_open_builder",
            Self::TokioOpenBuilder => "tokio_open_builder",
            Self::TempfileBuilder => "tempfile_builder",
            Self::SameReceiver => "same_receiver",
            Self::FirstArgument => "first_argument",
        }
    }
}

/// Stable filesystem-operation class.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    /// Open a root, file, directory, or builder.
    Open,
    /// Create a file or directory.
    Create,
    /// Enable or perform truncation.
    Truncate,
    /// Enable or perform append access.
    Append,
    /// Write bytes or structured output.
    Write,
    /// Change a file's length.
    SetLength,
    /// Change filesystem permissions.
    Permissions,
    /// Flush a buffered writer.
    Flush,
    /// Synchronize file or directory state.
    Sync,
    /// Persist a temporary file.
    Persist,
    /// Rename or atomically publish an entry.
    Rename,
    /// Publish an entry using a link operation.
    Link,
    /// Remove a file or directory.
    Remove,
}

impl OperationKind {
    pub(crate) const fn token(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Create => "create",
            Self::Truncate => "truncate",
            Self::Append => "append",
            Self::Write => "write",
            Self::SetLength => "set_length",
            Self::Permissions => "permissions",
            Self::Flush => "flush",
            Self::Sync => "sync",
            Self::Persist => "persist",
            Self::Rename => "rename",
            Self::Link => "link",
            Self::Remove => "remove",
        }
    }
}

/// Discovery hint independent of reviewed per-occurrence classification.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WriterRole {
    /// Root acquisition, open, create, or builder configuration.
    RootOpen,
    /// Mutation through an existing handle.
    HandleMutation,
    /// Rename, link, or temporary-file persistence.
    Publication,
    /// Permission mutation.
    Permissions,
    /// Flush or durability synchronization.
    Durability,
    /// Removal or rollback cleanup.
    Cleanup,
    /// The registered sink is commonly used as a shared primitive.
    SharedPrimitive,
    /// The registered sink is commonly a lexical false positive.
    FalsePositive,
}

impl WriterRole {
    pub(crate) const fn token(self) -> &'static str {
        match self {
            Self::RootOpen => "root_open",
            Self::HandleMutation => "handle_mutation",
            Self::Publication => "publication",
            Self::Permissions => "permissions",
            Self::Durability => "durability",
            Self::Cleanup => "cleanup",
            Self::SharedPrimitive => "shared_primitive",
            Self::FalsePositive => "false_positive",
        }
    }
}

/// Ecosystem that owns a registered sink.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SinkOrigin {
    /// Rust standard library.
    Standard,
    /// Tokio filesystem or asynchronous I/O.
    Tokio,
    /// Rustix descriptor-relative filesystem API.
    Rustix,
    /// Tempfile creation or persistence API.
    Tempfile,
    /// A registered project-private root or wrapper.
    ProjectWrapper,
    /// An explicitly reviewed non-writer match.
    Reviewed,
}

impl SinkOrigin {
    pub(crate) const fn token(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Tokio => "tokio",
            Self::Rustix => "rustix",
            Self::Tempfile => "tempfile",
            Self::ProjectWrapper => "project_wrapper",
            Self::Reviewed => "reviewed",
        }
    }
}

/// How a writer candidate was discovered.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SinkDiscovery {
    /// Qualified or imported function call.
    Function,
    /// Method call whose receiver flow was resolved.
    Method,
    /// Registered macro invocation.
    MacroInvocation,
    /// Raw registered sink-name token inside another macro.
    MacroToken,
}

impl SinkDiscovery {
    pub(crate) const fn token(self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Method => "method",
            Self::MacroInvocation => "macro_invocation",
            Self::MacroToken => "macro_token",
        }
    }
}

/// Complete stable writer-operation identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct WriterOperationId(Digest);

impl WriterOperationId {
    pub(crate) const fn new(digest: Digest) -> Self {
        Self(digest)
    }

    /// Return the complete identity digest.
    #[must_use]
    pub const fn digest(self) -> Digest {
        self.0
    }
}

/// One resolved writer operation.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct WriterOperation {
    pub(crate) id: WriterOperationId,
    pub(crate) path: RepositoryPath,
    pub(crate) span: ByteSpan,
    pub(crate) enclosing_item: Digest,
    pub(crate) normalized_call: Digest,
    pub(crate) sink: WriterToken,
    pub(crate) kind: OperationKind,
    pub(crate) role: WriterRole,
    pub(crate) discovery: SinkDiscovery,
    pub(crate) ordinal: u32,
}

impl WriterOperation {
    /// Return the stable operation identity.
    #[must_use]
    pub const fn id(&self) -> WriterOperationId {
        self.id
    }

    /// Return the repository-relative source path.
    #[must_use]
    pub const fn path(&self) -> &RepositoryPath {
        &self.path
    }

    /// Return the exact source byte span.
    #[must_use]
    pub const fn span(&self) -> ByteSpan {
        self.span
    }

    /// Return the enclosing-item identity digest.
    #[must_use]
    pub const fn enclosing_item(&self) -> Digest {
        self.enclosing_item
    }

    /// Return the normalized-call digest.
    #[must_use]
    pub const fn normalized_call(&self) -> Digest {
        self.normalized_call
    }

    /// Return the registered sink identifier.
    #[must_use]
    pub const fn sink(&self) -> &WriterToken {
        &self.sink
    }

    /// Return the operation class.
    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        self.kind
    }

    /// Return the inventory role.
    #[must_use]
    pub const fn role(&self) -> WriterRole {
        self.role
    }

    /// Return how the sink was discovered.
    #[must_use]
    pub const fn discovery(&self) -> SinkDiscovery {
        self.discovery
    }

    /// Return the ordinal within identical operation occurrences.
    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }
}
