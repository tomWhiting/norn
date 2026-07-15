//! Closed-schema validation for retained P1 evidence artifacts.

mod authoring;
mod authority;
mod contract;
mod evidence_document;
mod gate_document;
mod gate_evidence;
mod json;
mod model;
mod path_policy;
mod protocol;
mod protocol_json_schema;
mod protocol_literals;
mod protocol_schema;
mod registry_document;
mod run_local;
mod scan;
mod tool_inventory;
mod traceability;
mod validate;

pub use crate::finding::ArtifactIdentity;
pub use authoring::RedactionAuthoringError;
pub use authority::{RedactionRegistry, redaction_schema_digest};
pub use model::{
    ArtifactFamily, ArtifactRegistration, ObservationRegistration, ObservationSource, PublicUrl,
    RegistrationError, SentinelClass, SyntheticPurpose, SyntheticRegistration,
};
pub use registry_document::{RegistryDocumentError, RegistryEncodeError};
pub use validate::{RedactionCode, RedactionViolation, validate_retained_artifacts};

/// Iterate over the complete compiled P1 evidence-tool path inventory.
pub fn p1_evidence_tool_paths() -> impl ExactSizeIterator<Item = &'static str> {
    path_policy::evidence_tool_paths()
}
