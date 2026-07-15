//! Built-in registry assembly.

mod external;
mod project;
mod standard;
mod support;

use super::{RegistryError, SinkRegistry};
use crate::writers::model::FlowClass;

/// Construct the versioned built-in writer-sink registry.
pub fn builtin_sink_registry() -> Result<SinkRegistry, RegistryError> {
    let mut specs = Vec::new();
    standard::add(&mut specs);
    external::add(&mut specs);
    project::add(&mut specs)?;
    SinkRegistry::try_new(crate::writers::model::WRITER_SCHEMA_VERSION, specs)
}

pub(super) fn is_reviewed_non_writer_function(path: &str) -> bool {
    standard::is_reviewed_non_writer_function(path)
}

pub(super) fn reviewed_authority_function(path: &str) -> Option<FlowClass> {
    standard::reviewed_authority_function(path)
}

pub(super) fn reviewed_authority_method(name: &str, flow: FlowClass) -> Option<FlowClass> {
    standard::reviewed_authority_method(name, flow)
}

pub(super) const fn known_writer_namespaces() -> &'static [&'static str] {
    standard::known_writer_namespaces()
}

pub(super) const fn reviewed_non_writer_functions() -> &'static [&'static str] {
    standard::reviewed_non_writer_functions()
}

pub(super) const fn reviewed_authority_functions() -> &'static [(&'static str, FlowClass)] {
    standard::reviewed_authority_functions()
}

pub(super) const fn reviewed_authority_methods() -> &'static [(&'static str, FlowClass, FlowClass)]
{
    standard::reviewed_authority_methods()
}
