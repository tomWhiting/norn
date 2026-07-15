//! Typed conversion from canonical writer analysis into policy findings.

use thiserror::Error;

use super::{
    DefinitionSpec, RegistryError, SinkRegistry, SinkSpec, WriterInventory, WriterToken,
    builtin_sink_registry,
};
use crate::digest::{Digest, digest_bytes};
use crate::finding::{Finding, RepositoryFinding, UnknownWriterIssue};

/// Convert one canonical writer inventory into stable policy findings.
///
/// Unresolved source candidates retain their validated repository path and byte
/// span. Required sinks that were not observed are located through the exact
/// definition authority in Norn's built-in reviewed registry. No source bytes
/// or snippets cross this boundary.
///
/// # Errors
///
/// Returns a closed error if the inventory does not match the reviewed
/// registry or an unobserved sink cannot be resolved to exactly one required,
/// definition-backed registry row.
pub fn canonical_writer_findings(
    inventory: &WriterInventory,
) -> Result<Vec<Finding>, WriterFindingError> {
    if !inventory.has_valid_metadata() {
        return Err(WriterFindingError::InventoryMetadata);
    }
    let registry = builtin_sink_registry().map_err(WriterFindingError::ReviewedRegistry)?;
    if inventory.registry_digest() != registry.digest() {
        return Err(WriterFindingError::RegistryIdentity);
    }

    let mut findings = inventory
        .candidates()
        .iter()
        .map(|unknown| {
            Finding::repository(
                unknown.path().clone(),
                Some(unknown.span()),
                RepositoryFinding::UnknownWriterSink {
                    fingerprint: unknown.id().digest(),
                    issue: unknown.reason().into(),
                },
            )
        })
        .collect::<Vec<_>>();
    for sink in inventory.unobserved_required_sinks() {
        let (spec, definition) = required_definition_spec(&registry, sink)?;
        findings.push(Finding::repository(
            definition.source().clone(),
            None,
            RepositoryFinding::UnknownWriterSink {
                fingerprint: required_sink_fingerprint(spec.id()),
                issue: UnknownWriterIssue::UnobservedRequiredSink,
            },
        ));
    }
    findings.sort();
    Ok(findings)
}

fn required_definition_spec<'a>(
    registry: &'a SinkRegistry,
    sink: &WriterToken,
) -> Result<(&'a SinkSpec, &'a DefinitionSpec), WriterFindingError> {
    let spec = registry
        .specs()
        .iter()
        .find(|spec| spec.id() == sink)
        .ok_or(WriterFindingError::RequiredSinkMissing)?;
    if !spec.required_observation() {
        return Err(WriterFindingError::SinkDoesNotRequireObservation);
    }
    let definition = spec
        .definition()
        .ok_or(WriterFindingError::RequiredSinkDefinition)?;
    Ok((spec, definition))
}

fn required_sink_fingerprint(sink: &WriterToken) -> Digest {
    let mut framed = Vec::with_capacity(64);
    framed.extend_from_slice(digest_bytes(b"norn-unobserved-required-writer-sink-1").as_bytes());
    framed.extend_from_slice(digest_bytes(sink.as_str().as_bytes()).as_bytes());
    digest_bytes(&framed)
}

/// Failure converting writer-analysis state into stable policy findings.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WriterFindingError {
    /// The built-in reviewed registry could not be constructed.
    #[error("built-in reviewed writer registry is invalid")]
    ReviewedRegistry(#[source] RegistryError),
    /// Writer inventory metadata or its source digest is internally stale.
    #[error("writer inventory metadata is invalid")]
    InventoryMetadata,
    /// The inventory was produced using another registry identity.
    #[error("writer inventory does not use the built-in reviewed registry")]
    RegistryIdentity,
    /// An unobserved sink ID is absent from the reviewed registry.
    #[error("unobserved required sink is absent from the reviewed registry")]
    RequiredSinkMissing,
    /// The named row does not carry a required-observation contract.
    #[error("unobserved sink is not required by the reviewed registry")]
    SinkDoesNotRequireObservation,
    /// The required row has no exact definition source authority.
    #[error("unobserved required sink has no definition authority")]
    RequiredSinkDefinition,
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;
    use crate::writers::{FlowClass, OperationKind, SinkOrigin, WriterRole};

    type TestResult = Result<(), Box<dyn Error>>;

    #[test]
    fn required_sink_lookup_fails_closed_for_every_unmapped_shape() -> TestResult {
        let schema_version = builtin_sink_registry()?.schema_version();
        let ordinary = SinkSpec::function(
            "fixture.ordinary",
            "fixture::ordinary",
            OperationKind::Write,
            WriterRole::HandleMutation,
            FlowClass::None,
            SinkOrigin::Reviewed,
        )?;
        let ordinary_id = ordinary.id().clone();
        let registry = SinkRegistry::try_new(schema_version, vec![ordinary])?;
        assert!(matches!(
            required_definition_spec(&registry, &ordinary_id),
            Err(WriterFindingError::SinkDoesNotRequireObservation)
        ));

        let missing = WriterToken::parse("fixture.missing")?;
        assert!(matches!(
            required_definition_spec(&registry, &missing),
            Err(WriterFindingError::RequiredSinkMissing)
        ));

        let definitionless = SinkSpec::function(
            "fixture.definitionless",
            "fixture::definitionless",
            OperationKind::Write,
            WriterRole::HandleMutation,
            FlowClass::None,
            SinkOrigin::Reviewed,
        )?
        .require_observation();
        let definitionless_id = definitionless.id().clone();
        let registry = SinkRegistry::try_new(schema_version, vec![definitionless])?;
        assert!(matches!(
            required_definition_spec(&registry, &definitionless_id),
            Err(WriterFindingError::RequiredSinkDefinition)
        ));
        Ok(())
    }

    #[test]
    fn required_sink_fingerprints_are_domain_separated_and_identity_bound() -> TestResult {
        let first = WriterToken::parse("fixture.first")?;
        let second = WriterToken::parse("fixture.second")?;
        assert_eq!(
            required_sink_fingerprint(&first),
            required_sink_fingerprint(&first)
        );
        assert_ne!(
            required_sink_fingerprint(&first),
            required_sink_fingerprint(&second)
        );
        assert_ne!(
            required_sink_fingerprint(&first),
            digest_bytes(first.as_str().as_bytes())
        );
        Ok(())
    }
}
