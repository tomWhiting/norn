//! Compact helpers for built-in registry declarations.

use super::super::{ReceiverConstraint, SinkSelector, SinkSpec};
use crate::writers::model::{FlowClass, OperationKind, SinkOrigin, WriterRole};

pub(super) type FunctionDefinition = (
    &'static str,
    &'static str,
    OperationKind,
    WriterRole,
    FlowClass,
);
pub(super) type MethodDefinition = (&'static str, &'static str, OperationKind, WriterRole);

pub(super) fn function(definition: FunctionDefinition, origin: SinkOrigin) -> SinkSpec {
    SinkSpec::builtin(
        definition.0,
        SinkSelector::Function {
            path: definition.1.to_owned(),
        },
        definition.2,
        definition.3,
        definition.4,
        origin,
    )
}

pub(super) fn method(
    definition: MethodDefinition,
    receiver: ReceiverConstraint,
    returns: FlowClass,
    origin: SinkOrigin,
) -> SinkSpec {
    SinkSpec::builtin(
        definition.0,
        SinkSelector::Method {
            name: definition.1.to_owned(),
            receiver,
        },
        definition.2,
        definition.3,
        returns,
        origin,
    )
}
