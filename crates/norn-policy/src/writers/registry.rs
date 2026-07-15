//! Versioned closed writer-sink registry.

mod auxiliary;
mod builtin;
mod definition;
mod spec;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;

use thiserror::Error;

use crate::digest::{Digest, digest_bytes};

use super::model::{FlowClass, SinkOrigin, WRITER_SCHEMA_VERSION, WriterToken, WriterTokenError};
use auxiliary::validate_auxiliary_authorities;

pub use builtin::builtin_sink_registry;
pub(crate) use definition::DefinitionReceiver;
pub use definition::DefinitionSpec;
pub use spec::SinkSpec;

/// Receiver authority required by a registered method sink.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ReceiverConstraint {
    /// Any already tracked root, handle, or builder.
    AnyTracked,
    /// Project-private root authority.
    RootAuthority,
    /// Writable file or stream handle.
    WritableHandle,
    /// Temporary-file handle supporting persistence.
    TemporaryHandle,
    /// Standard-library open-options builder.
    StandardOpenBuilder,
    /// Tokio open-options builder.
    TokioOpenBuilder,
    /// Tempfile builder.
    TempfileBuilder,
}

impl ReceiverConstraint {
    pub(crate) const fn accepts(self, flow: FlowClass) -> bool {
        match self {
            Self::AnyTracked => !matches!(flow, FlowClass::None),
            Self::RootAuthority => matches!(flow, FlowClass::RootAuthority),
            Self::WritableHandle => {
                matches!(flow, FlowClass::WritableHandle | FlowClass::TemporaryHandle)
            }
            Self::TemporaryHandle => matches!(flow, FlowClass::TemporaryHandle),
            Self::StandardOpenBuilder => matches!(flow, FlowClass::StandardOpenBuilder),
            Self::TokioOpenBuilder => matches!(flow, FlowClass::TokioOpenBuilder),
            Self::TempfileBuilder => matches!(flow, FlowClass::TempfileBuilder),
        }
    }

    const fn token(self) -> &'static str {
        match self {
            Self::AnyTracked => "any_tracked",
            Self::RootAuthority => "root_authority",
            Self::WritableHandle => "writable_handle",
            Self::TemporaryHandle => "temporary_handle",
            Self::StandardOpenBuilder => "standard_open_builder",
            Self::TokioOpenBuilder => "tokio_open_builder",
            Self::TempfileBuilder => "tempfile_builder",
        }
    }
}

/// Exact syntax selector for a registered sink.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SinkSelector {
    /// Qualified or imported function/static-method path.
    Function {
        /// Qualified registered path.
        path: String,
    },
    /// Method name plus required tracked receiver authority.
    Method {
        /// Registered method name.
        name: String,
        /// Required tracked receiver authority.
        receiver: ReceiverConstraint,
    },
    /// Qualified or imported macro path.
    Macro {
        /// Qualified registered macro path.
        path: String,
    },
}

impl SinkSelector {
    pub(crate) fn terminal(&self) -> &str {
        match self {
            Self::Function { path } | Self::Macro { path } => {
                path.rsplit("::").next().unwrap_or(path)
            }
            Self::Method { name, .. } => name,
        }
    }

    fn key(&self) -> String {
        match self {
            Self::Function { path } => format!("function:{path}"),
            Self::Method { name, receiver } => {
                format!("method:{}:{name}", receiver.token())
            }
            Self::Macro { path } => format!("macro:{path}"),
        }
    }
}

/// Validated versioned registry of exact writer sinks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SinkRegistry {
    schema_version: u32,
    specs: Vec<SinkSpec>,
}

impl SinkRegistry {
    /// Construct and validate a closed registry.
    ///
    /// # Errors
    ///
    /// Rejects unsupported versions, duplicate IDs, and duplicate selectors.
    pub fn try_new(schema_version: u32, specs: Vec<SinkSpec>) -> Result<Self, RegistryError> {
        let registry = Self {
            schema_version,
            specs,
        };
        registry.validate()?;
        Ok(registry)
    }

    /// Return the registry schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Borrow registered sinks in stable declaration order.
    #[must_use]
    pub fn specs(&self) -> &[SinkSpec] {
        &self.specs
    }

    /// Validate registry identity and selector uniqueness.
    ///
    /// # Errors
    ///
    /// Returns a closed registry-integrity error.
    pub fn validate(&self) -> Result<(), RegistryError> {
        if self.schema_version != WRITER_SCHEMA_VERSION {
            return Err(RegistryError::SchemaVersion);
        }
        validate_auxiliary_authorities(
            builtin::known_writer_namespaces(),
            builtin::reviewed_non_writer_functions(),
            builtin::reviewed_authority_functions(),
            builtin::reviewed_authority_methods(),
        )?;
        let mut ids = BTreeSet::new();
        let mut selectors = BTreeSet::new();
        let mut definitions = BTreeSet::new();
        for spec in &self.specs {
            validate_spec(spec)?;
            if !ids.insert(spec.id().clone()) {
                return Err(RegistryError::DuplicateId);
            }
            if !selectors.insert(selector_key(spec)) {
                return Err(RegistryError::DuplicateSelector);
            }
            if spec.origin() == SinkOrigin::ProjectWrapper && spec.definition().is_none() {
                return Err(RegistryError::ProjectDefinitionRequired);
            }
            if spec.origin() != SinkOrigin::ProjectWrapper && spec.definition().is_some() {
                return Err(RegistryError::UnexpectedDefinition);
            }
            if let Some(definition) = spec.definition()
                && !definitions.insert(definition)
            {
                return Err(RegistryError::DuplicateDefinition);
            }
        }
        Ok(())
    }

    /// Return a deterministic digest of every registry semantic.
    #[must_use]
    pub fn digest(&self) -> Digest {
        let mut specs: Vec<&SinkSpec> = self.specs.iter().collect();
        specs.sort_by(|left, right| left.id().cmp(right.id()));
        let mut framed = Vec::new();
        semantic_field(&mut framed, b"norn-writer-registry-1");
        semantic_field(&mut framed, &self.schema_version.to_be_bytes());
        for spec in specs {
            semantic_field(&mut framed, spec.id().as_str().as_bytes());
            semantic_field(&mut framed, selector_key(spec).as_bytes());
            semantic_field(&mut framed, spec.kind().token().as_bytes());
            semantic_field(&mut framed, spec.role().token().as_bytes());
            semantic_field(&mut framed, spec.returns().token().as_bytes());
            semantic_field(&mut framed, spec.origin().token().as_bytes());
            framed.push(u8::from(spec.required_observation()));
            if let Some(definition) = spec.definition() {
                semantic_field(&mut framed, definition.source().as_str().as_bytes());
                semantic_field(&mut framed, definition.item().as_bytes());
                semantic_field(&mut framed, definition.signature().as_bytes());
                semantic_field(&mut framed, definition.implementation().as_bytes());
            }
        }
        for namespace in builtin::known_writer_namespaces() {
            semantic_field(&mut framed, b"known_namespace");
            semantic_field(&mut framed, namespace.as_bytes());
        }
        for function in builtin::reviewed_non_writer_functions() {
            semantic_field(&mut framed, b"reviewed_non_writer");
            semantic_field(&mut framed, function.as_bytes());
        }
        for (function, returns) in builtin::reviewed_authority_functions() {
            semantic_field(&mut framed, b"reviewed_authority_function");
            semantic_field(&mut framed, function.as_bytes());
            semantic_field(&mut framed, returns.token().as_bytes());
        }
        for (method, receiver, returns) in builtin::reviewed_authority_methods() {
            semantic_field(&mut framed, b"reviewed_authority_method");
            semantic_field(&mut framed, method.as_bytes());
            semantic_field(&mut framed, receiver.token().as_bytes());
            semantic_field(&mut framed, returns.token().as_bytes());
        }
        digest_bytes(&framed)
    }

    pub(crate) fn function(
        &self,
        path: &str,
        source: &crate::path::RepositoryPath,
        local_item: &str,
    ) -> Option<&SinkSpec> {
        self.specs.iter().find(|spec| match spec.selector() {
            SinkSelector::Function { path: registered } => {
                spec.definition().map_or(registered == path, |definition| {
                    (registered.contains("::") && registered == path)
                        || (definition.source() == source && definition.item() == local_item)
                })
            }
            SinkSelector::Method { .. } | SinkSelector::Macro { .. } => false,
        })
    }

    pub(crate) fn method(
        &self,
        name: &str,
        flow: FlowClass,
        provenance: Option<&DefinitionReceiver>,
    ) -> Option<&SinkSpec> {
        let mut matches = self.specs.iter().filter(|spec| {
            let selector_matches = matches!(
                spec.selector(),
                SinkSelector::Method { name: registered, receiver }
                    if registered == name && receiver.accepts(flow)
            );
            selector_matches
                && spec
                    .definition()
                    .and_then(DefinitionSpec::receiver)
                    .as_ref()
                    .is_none_or(|receiver| Some(receiver) == provenance)
        });
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }

    pub(crate) fn definition_receiver(
        &self,
        source: &crate::path::RepositoryPath,
        resolved: &str,
        local: &str,
    ) -> Option<DefinitionReceiver> {
        let mut matches = self.specs.iter().filter_map(|spec| {
            let definition = spec.definition()?;
            let public_receiver_matches = match spec.selector() {
                SinkSelector::Function { path } => {
                    path.rsplit_once("::").is_some_and(|(receiver, _)| {
                        (receiver == resolved || receiver == local)
                            && (receiver.starts_with("crate::") || definition.source() == source)
                    })
                }
                SinkSelector::Method { .. } | SinkSelector::Macro { .. } => false,
            };
            if public_receiver_matches || definition.matches_receiver_type(source, resolved, local)
            {
                definition.receiver()
            } else {
                None
            }
        });
        let first = matches.next()?;
        matches.all(|candidate| candidate == first).then_some(first)
    }

    pub(crate) fn macro_sink(&self, path: &str) -> Option<&SinkSpec> {
        self.specs.iter().find(|spec| {
            matches!(spec.selector(), SinkSelector::Macro { path: registered } if registered == path)
        })
    }

    pub(crate) fn has_method_name(&self, name: &str) -> bool {
        self.specs.iter().any(|spec| {
            matches!(spec.selector(), SinkSelector::Method { name: registered, .. } if registered == name)
        })
    }

    pub(crate) fn has_terminal(&self, name: &str) -> bool {
        self.specs
            .iter()
            .any(|spec| spec.selector().terminal() == name)
    }

    pub(crate) fn terminal_names(&self) -> BTreeSet<&str> {
        self.specs
            .iter()
            .map(|spec| spec.selector().terminal())
            .collect()
    }

    pub(crate) fn has_path_prefix(&self, prefix: &str) -> bool {
        let prefix = format!("{prefix}::");
        self.specs.iter().any(|spec| match spec.selector() {
            SinkSelector::Function { path } | SinkSelector::Macro { path } => {
                path.starts_with(&prefix)
            }
            SinkSelector::Method { .. } => false,
        })
    }

    pub(crate) fn is_function_candidate(&self, path: &str) -> bool {
        let Some((parent, _)) = path.rsplit_once("::") else {
            return false;
        };
        builtin::known_writer_namespaces().contains(&parent)
            || self.specs.iter().any(|spec| {
                matches!(
                    spec.selector(),
                    SinkSelector::Function { path } if path.starts_with(&format!("{parent}::"))
                )
            })
    }

    pub(crate) fn has_definition(&self, source: &crate::path::RepositoryPath, item: &str) -> bool {
        self.specs.iter().any(|spec| {
            spec.definition().is_some_and(|definition| {
                definition.source() == source && definition.item() == item
            })
        })
    }

    pub(crate) fn is_reviewed_non_writer_function(path: &str) -> bool {
        builtin::is_reviewed_non_writer_function(path)
    }

    pub(crate) fn reviewed_authority_function(path: &str) -> Option<FlowClass> {
        builtin::reviewed_authority_function(path)
    }

    pub(crate) fn reviewed_authority_method(name: &str, flow: FlowClass) -> Option<FlowClass> {
        builtin::reviewed_authority_method(name, flow)
    }
}

/// Closed writer-registry validation failures.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RegistryError {
    /// The registry schema version is unsupported.
    #[error("writer registry schema version is unsupported")]
    SchemaVersion,
    /// Two registry rows share a stable sink ID.
    #[error("writer registry contains a duplicate sink id")]
    DuplicateId,
    /// Two registry rows select the same call form.
    #[error("writer registry contains a duplicate selector")]
    DuplicateSelector,
    /// A Rust path is structurally invalid.
    #[error("writer registry Rust path is invalid")]
    RustPath,
    /// A Rust identifier is structurally invalid.
    #[error("writer registry Rust identifier is invalid")]
    Identifier,
    /// A project wrapper omitted exact definition authority.
    #[error("project wrapper registry row requires definition authority")]
    ProjectDefinitionRequired,
    /// A non-project sink unexpectedly carried project definition authority.
    #[error("non-project writer registry row cannot carry definition authority")]
    UnexpectedDefinition,
    /// Two registry rows claim the same exact project definition.
    #[error("writer registry contains a duplicate project definition")]
    DuplicateDefinition,
    /// A project definition repository path is invalid.
    #[error("project writer definition path is invalid")]
    DefinitionPath,
    /// A project definition is incomplete, invalid, or names another item.
    #[error("project writer definition authority is invalid")]
    DefinitionAuthority,
    /// A project selector and definition name disagree.
    #[error("project writer selector does not name its definition")]
    DefinitionSelector,
    /// A writer token is invalid.
    #[error("writer registry token is invalid")]
    Token(#[from] WriterTokenError),
    /// A built-in namespace or reviewed authority adapter is invalid.
    #[error("writer registry auxiliary authority is invalid")]
    AuxiliaryAuthority,
}

fn validate_rust_path(path: &str) -> Result<(), RegistryError> {
    if path.is_empty()
        || path
            .split("::")
            .any(|part| validate_identifier(part).is_err())
    {
        return Err(RegistryError::RustPath);
    }
    Ok(())
}

fn validate_identifier(identifier: &str) -> Result<(), RegistryError> {
    let mut bytes = identifier.bytes();
    let Some(first) = bytes.next() else {
        return Err(RegistryError::Identifier);
    };
    if !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(RegistryError::Identifier);
    }
    Ok(())
}

fn validate_definition_selector(
    selector: &str,
    definition: &DefinitionSpec,
) -> Result<(), RegistryError> {
    if selector.rsplit("::").next() != definition.item().rsplit("::").next() {
        return Err(RegistryError::DefinitionSelector);
    }
    Ok(())
}

fn validate_spec(spec: &SinkSpec) -> Result<(), RegistryError> {
    WriterToken::parse(spec.id().as_str())?;
    match spec.selector() {
        SinkSelector::Function { path } | SinkSelector::Macro { path } => {
            validate_rust_path(path)?;
        }
        SinkSelector::Method { name, .. } => validate_identifier(name)?,
    }
    if let Some(definition) = spec.definition() {
        validate_definition_selector(spec.selector().terminal(), definition)?;
    }
    Ok(())
}

fn selector_key(spec: &SinkSpec) -> String {
    let base = spec.selector().key();
    match (
        spec.selector(),
        spec.definition().and_then(DefinitionSpec::receiver),
    ) {
        (SinkSelector::Method { .. }, Some(receiver)) => {
            format!("{base}:definition:{}", receiver.key())
        }
        _ => base,
    }
}

fn semantic_field(output: &mut Vec<u8>, value: &[u8]) {
    let length = value.len().to_be_bytes();
    output.extend_from_slice(&[0_u8; 16][length.len()..]);
    output.extend_from_slice(&length);
    output.extend_from_slice(value);
}
