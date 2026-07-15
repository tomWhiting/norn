//! Pure multi-source writer scan orchestration.

mod finalize;
mod source;

use std::collections::BTreeSet;

use crate::digest::Digest;
use crate::path::RepositoryPath;
use crate::writers::imports::ReexportGraph;

use super::candidate::WriterCandidateForm;
use super::input::{WriterScanError, WriterSource};
use super::model::{
    OperationKind, SinkDiscovery, UnknownSinkReason, WRITER_ANALYZER_VERSION,
    WRITER_SCHEMA_VERSION, WriterInventory, WriterRole, WriterSourceIdentity, WriterToken,
    writer_source_inventory_digest,
};
use super::registry::SinkRegistry;

/// Analyze production Rust sources using one exact sink registry.
///
/// Input order does not affect operation ordering or stable IDs. Proved
/// `cfg(test)` source ranges are excluded by the shared Rust analyzer.
///
/// # Errors
///
/// Rejects duplicate source paths, invalid registry state, invalid Rust, and
/// identities that exceed their versioned integer representation.
pub fn analyze_writers(
    sources: &[WriterSource],
    registry: &SinkRegistry,
) -> Result<WriterInventory, WriterScanError> {
    registry.validate()?;
    let mut ordered: Vec<&WriterSource> = sources.iter().collect();
    ordered.sort_by(|left, right| left.path().cmp(right.path()));
    if ordered
        .windows(2)
        .any(|pair| pair[0].path() == pair[1].path())
    {
        return Err(WriterScanError::DuplicateSource);
    }
    let source_inventory: Vec<WriterSourceIdentity> = ordered
        .iter()
        .map(|source| WriterSourceIdentity::from_bytes(source.path().clone(), source.bytes()))
        .collect();
    let source_inventory_digest = writer_source_inventory_digest(&source_inventory);
    let registry_digest = registry.digest();
    let reexports = ReexportGraph::build(&ordered)?;
    let functions = source::FunctionIndex::build(&ordered, registry, &reexports)?;

    let mut operations = Vec::new();
    let mut candidates = Vec::new();
    let mut observed = BTreeSet::new();
    for source in ordered {
        source::scan_source(
            source,
            registry,
            &reexports,
            &functions,
            &mut operations,
            &mut candidates,
            &mut observed,
        )?;
    }
    let operations = finalize::operations(operations)?;
    let candidates = finalize::candidates(candidates)?;
    let unobserved_required_sinks = registry
        .specs()
        .iter()
        .filter(|spec| spec.required_observation() && !observed.contains(spec.id()))
        .map(|spec| spec.id().clone())
        .collect();
    Ok(WriterInventory {
        schema_version: WRITER_SCHEMA_VERSION,
        analyzer_version: WriterToken::from_static(WRITER_ANALYZER_VERSION),
        sources: source_inventory,
        source_inventory_digest,
        registry_digest,
        operations,
        candidates,
        unobserved_required_sinks,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RawOperation {
    pub(super) path: RepositoryPath,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) enclosing_item: Digest,
    pub(super) normalized_call: Digest,
    pub(super) sink: WriterToken,
    pub(super) kind: OperationKind,
    pub(super) role: WriterRole,
    pub(super) discovery: SinkDiscovery,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RawCandidate {
    pub(super) path: RepositoryPath,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) enclosing_item: Digest,
    pub(super) normalized_call: Digest,
    pub(super) candidate: WriterToken,
    pub(super) reason: UnknownSinkReason,
    pub(super) form: WriterCandidateForm,
}
