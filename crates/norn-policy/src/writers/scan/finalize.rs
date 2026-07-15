//! Stable multiset ordinals and writer identity digests.

use std::collections::BTreeMap;

use crate::digest::Digest;
use crate::finding::ByteSpan;

use super::{RawCandidate, RawOperation};
use crate::writers::candidate::{WriterCandidate, WriterCandidateSemantics};
use crate::writers::identity::{OperationIdentityInput, operation_id, operation_key};
use crate::writers::input::WriterScanError;
use crate::writers::model::WriterOperation;

pub(super) fn operations(
    mut raw: Vec<RawOperation>,
) -> Result<Vec<WriterOperation>, WriterScanError> {
    raw.sort_by_key(|operation| (operation_key(&identity_input(operation)), operation.start));
    let mut ordinals = BTreeMap::new();
    let mut operations = Vec::with_capacity(raw.len());
    for operation in raw {
        let input = identity_input(&operation);
        let key = operation_key(&input);
        let ordinal = next_ordinal(&mut ordinals, key.clone())?;
        let id = operation_id(&input, ordinal);
        operations.push(WriterOperation {
            id,
            path: operation.path,
            span: span(operation.start, operation.end)?,
            enclosing_item: operation.enclosing_item,
            normalized_call: operation.normalized_call,
            sink: operation.sink,
            kind: operation.kind,
            role: operation.role,
            discovery: operation.discovery,
            ordinal,
        });
    }
    operations.sort_by(|left, right| {
        (left.path(), left.span(), left.kind(), left.id()).cmp(&(
            right.path(),
            right.span(),
            right.kind(),
            right.id(),
        ))
    });
    Ok(operations)
}

pub(super) fn candidates(
    mut raw: Vec<RawCandidate>,
) -> Result<Vec<WriterCandidate>, WriterScanError> {
    raw.sort_by_key(|candidate| (candidate_key(candidate), candidate.start));
    let mut ordinals = BTreeMap::new();
    let mut candidates = Vec::with_capacity(raw.len());
    for candidate in raw {
        let key = candidate_key(&candidate);
        let ordinal = next_candidate_ordinal(&mut ordinals, key)?;
        let semantics = WriterCandidateSemantics::new(
            candidate.enclosing_item,
            candidate.normalized_call,
            candidate.candidate,
            candidate.reason,
            candidate.form,
        );
        candidates.push(WriterCandidate::new(
            candidate.path,
            span(candidate.start, candidate.end)?,
            semantics,
            ordinal,
        ));
    }
    candidates.sort_by(|left, right| {
        (left.path(), left.span(), left.reason(), left.id()).cmp(&(
            right.path(),
            right.span(),
            right.reason(),
            right.id(),
        ))
    });
    Ok(candidates)
}

fn identity_input(operation: &RawOperation) -> OperationIdentityInput<'_> {
    OperationIdentityInput {
        path: &operation.path,
        enclosing_item: operation.enclosing_item,
        normalized_call: operation.normalized_call,
        sink: &operation.sink,
        kind: operation.kind,
        role: operation.role,
        discovery: operation.discovery,
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CandidateKey {
    path: crate::path::RepositoryPath,
    enclosing_item: Digest,
    normalized_call: Digest,
    candidate: crate::writers::WriterToken,
    reason: crate::writers::UnknownSinkReason,
    form: crate::writers::WriterCandidateForm,
}

fn candidate_key(candidate: &RawCandidate) -> CandidateKey {
    CandidateKey {
        path: candidate.path.clone(),
        enclosing_item: candidate.enclosing_item,
        normalized_call: candidate.normalized_call,
        candidate: candidate.candidate.clone(),
        reason: candidate.reason,
        form: candidate.form,
    }
}

fn next_ordinal(
    ordinals: &mut BTreeMap<Vec<u8>, u32>,
    key: Vec<u8>,
) -> Result<u32, WriterScanError> {
    let ordinal = ordinals.entry(key).or_insert(0);
    let current = *ordinal;
    *ordinal = ordinal.checked_add(1).ok_or(WriterScanError::Ordinal)?;
    Ok(current)
}

fn next_candidate_ordinal(
    ordinals: &mut BTreeMap<CandidateKey, u32>,
    key: CandidateKey,
) -> Result<u32, WriterScanError> {
    let ordinal = ordinals.entry(key).or_insert(0);
    let current = *ordinal;
    *ordinal = ordinal.checked_add(1).ok_or(WriterScanError::Ordinal)?;
    Ok(current)
}

fn span(start: usize, end: usize) -> Result<ByteSpan, WriterScanError> {
    let Ok(start) = u64::try_from(start) else {
        return Err(WriterScanError::Offset);
    };
    let Ok(end) = u64::try_from(end) else {
        return Err(WriterScanError::Offset);
    };
    ByteSpan::new(start, end).map_err(WriterScanError::from)
}
