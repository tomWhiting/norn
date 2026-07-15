//! One validated writer sink semantic.

use super::{
    DefinitionSpec, ReceiverConstraint, RegistryError, SinkSelector, validate_definition_selector,
    validate_identifier, validate_rust_path,
};
use crate::writers::model::{FlowClass, OperationKind, SinkOrigin, WriterRole, WriterToken};

/// One closed sink or registered project-wrapper semantic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SinkSpec {
    id: WriterToken,
    selector: SinkSelector,
    kind: OperationKind,
    role: WriterRole,
    returns: FlowClass,
    origin: SinkOrigin,
    required_observation: bool,
    definition: Option<DefinitionSpec>,
}

impl SinkSpec {
    /// Construct a function or static-method sink.
    ///
    /// # Errors
    ///
    /// Rejects invalid identifiers or Rust paths.
    pub fn function(
        id: &str,
        path: &str,
        kind: OperationKind,
        role: WriterRole,
        returns: FlowClass,
        origin: SinkOrigin,
    ) -> Result<Self, RegistryError> {
        validate_rust_path(path)?;
        Ok(Self {
            id: WriterToken::parse(id)?,
            selector: SinkSelector::Function {
                path: path.to_owned(),
            },
            kind,
            role,
            returns,
            origin,
            required_observation: false,
            definition: None,
        })
    }

    /// Construct a method sink constrained by tracked receiver authority.
    ///
    /// # Errors
    ///
    /// Rejects invalid identifiers.
    pub fn method(
        id: &str,
        name: &str,
        receiver: ReceiverConstraint,
        kind: OperationKind,
        role: WriterRole,
        returns: FlowClass,
        origin: SinkOrigin,
    ) -> Result<Self, RegistryError> {
        validate_identifier(name)?;
        Ok(Self {
            id: WriterToken::parse(id)?,
            selector: SinkSelector::Method {
                name: name.to_owned(),
                receiver,
            },
            kind,
            role,
            returns,
            origin,
            required_observation: false,
            definition: None,
        })
    }

    /// Construct a registered macro sink.
    ///
    /// # Errors
    ///
    /// Rejects invalid identifiers or Rust paths.
    pub fn macro_sink(
        id: &str,
        path: &str,
        kind: OperationKind,
        role: WriterRole,
        origin: SinkOrigin,
    ) -> Result<Self, RegistryError> {
        validate_rust_path(path)?;
        Ok(Self {
            id: WriterToken::parse(id)?,
            selector: SinkSelector::Macro {
                path: path.to_owned(),
            },
            kind,
            role,
            returns: FlowClass::None,
            origin,
            required_observation: false,
            definition: None,
        })
    }

    /// Construct a definition-backed project wrapper function.
    ///
    /// # Errors
    ///
    /// Rejects invalid identifiers, paths, or a selector/definition name mismatch.
    pub fn project_function(
        id: &str,
        path: &str,
        definition: DefinitionSpec,
        kind: OperationKind,
        role: WriterRole,
        returns: FlowClass,
    ) -> Result<Self, RegistryError> {
        validate_rust_path(path)?;
        validate_definition_selector(path, &definition)?;
        Ok(Self {
            id: WriterToken::parse(id)?,
            selector: SinkSelector::Function {
                path: path.to_owned(),
            },
            kind,
            role,
            returns,
            origin: SinkOrigin::ProjectWrapper,
            required_observation: true,
            definition: Some(definition),
        })
    }

    /// Construct a definition-backed project wrapper method.
    ///
    /// # Errors
    ///
    /// Rejects invalid identifiers or a selector/definition name mismatch.
    pub fn project_method(
        id: &str,
        name: &str,
        receiver: ReceiverConstraint,
        definition: DefinitionSpec,
        kind: OperationKind,
        role: WriterRole,
        returns: FlowClass,
    ) -> Result<Self, RegistryError> {
        validate_identifier(name)?;
        validate_definition_selector(name, &definition)?;
        if definition.receiver().is_none() {
            return Err(RegistryError::DefinitionSelector);
        }
        Ok(Self {
            id: WriterToken::parse(id)?,
            selector: SinkSelector::Method {
                name: name.to_owned(),
                receiver,
            },
            kind,
            role,
            returns,
            origin: SinkOrigin::ProjectWrapper,
            required_observation: true,
            definition: Some(definition),
        })
    }

    /// Require at least one occurrence when analyzing a complete source set.
    #[must_use]
    pub const fn require_observation(mut self) -> Self {
        self.required_observation = true;
        self
    }

    /// Return the stable sink identifier.
    #[must_use]
    pub const fn id(&self) -> &WriterToken {
        &self.id
    }

    /// Return the exact selector.
    #[must_use]
    pub const fn selector(&self) -> &SinkSelector {
        &self.selector
    }

    /// Return the operation kind.
    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        self.kind
    }

    /// Return the inventory role.
    #[must_use]
    pub const fn role(&self) -> WriterRole {
        self.role
    }

    /// Return the propagated flow semantics.
    #[must_use]
    pub const fn returns(&self) -> FlowClass {
        self.returns
    }

    /// Return the owning ecosystem.
    #[must_use]
    pub const fn origin(&self) -> SinkOrigin {
        self.origin
    }

    /// Return exact project-wrapper definition authority, when applicable.
    #[must_use]
    pub const fn definition(&self) -> Option<&DefinitionSpec> {
        self.definition.as_ref()
    }

    pub(crate) const fn required_observation(&self) -> bool {
        self.required_observation
    }

    pub(crate) fn builtin(
        id: impl Into<String>,
        selector: SinkSelector,
        kind: OperationKind,
        role: WriterRole,
        returns: FlowClass,
        origin: SinkOrigin,
    ) -> Self {
        Self {
            id: WriterToken::from_trusted(id),
            selector,
            kind,
            role,
            returns,
            origin,
            required_observation: false,
            definition: None,
        }
    }
}
