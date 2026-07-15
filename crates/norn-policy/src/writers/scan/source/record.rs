//! Canonical operation and unresolved-candidate recording.

use tree_sitter::Node;

use super::{RecordedSink, Scanner};
use crate::writers::input::WriterScanError;
use crate::writers::model::{SinkDiscovery, UnknownSinkReason, WriterToken};
use crate::writers::syntax::{enclosing_item_digest, function_name, normalized_node_digest};
use crate::writers::{WriterCandidateForm, scan::RawCandidate, scan::RawOperation};

impl Scanner<'_, '_, '_> {
    pub(super) fn operation(
        &mut self,
        node: Node<'_>,
        sink: RecordedSink,
        discovery: SinkDiscovery,
    ) {
        if !sink.definition_bound {
            self.observed.insert(sink.id.clone());
        }
        self.operations.push(RawOperation {
            path: self.path.clone(),
            start: node.start_byte(),
            end: node.end_byte(),
            enclosing_item: enclosing_item_digest(node, self.bytes),
            normalized_call: normalized_node_digest(node, self.bytes),
            sink: sink.id,
            kind: sink.kind,
            role: sink.role,
            discovery,
        });
    }

    pub(super) fn unknown(
        &mut self,
        node: Node<'_>,
        candidate: &str,
        reason: UnknownSinkReason,
        form: WriterCandidateForm,
    ) -> Result<(), WriterScanError> {
        self.candidates.push(RawCandidate {
            path: self.path.clone(),
            start: node.start_byte(),
            end: node.end_byte(),
            enclosing_item: enclosing_item_digest(node, self.bytes),
            normalized_call: normalized_node_digest(node, self.bytes),
            candidate: WriterToken::parse(candidate.to_ascii_lowercase())?,
            reason,
            form,
        });
        Ok(())
    }

    pub(super) fn authority_candidate(
        &mut self,
        node: Node<'_>,
        candidate: &str,
        reason: UnknownSinkReason,
    ) -> Result<(), WriterScanError> {
        self.unknown(
            node,
            candidate,
            reason,
            WriterCandidateForm::AuthorityEscape,
        )
    }

    pub(super) fn new_wrapper_candidate(&mut self) -> Result<(), WriterScanError> {
        if self.container.kind() != "function_item" {
            return Ok(());
        }
        if !self.has_implicit_return_authority() || self.registered_container() {
            return Ok(());
        }
        let Some(name) = function_name(self.container, self.bytes) else {
            return Ok(());
        };
        if let Some(name_node) = self.container.child_by_field_name("name") {
            self.unknown(
                name_node,
                &name,
                UnknownSinkReason::NewWrapperCandidate,
                WriterCandidateForm::WrapperDefinition,
            )?;
        }
        Ok(())
    }
}
