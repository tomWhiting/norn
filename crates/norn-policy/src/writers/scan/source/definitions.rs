//! Exact project-wrapper definition observation.

use tree_sitter::Node;

use super::support::definition_paths;
use super::{RawCandidate, ScanOutput};
use crate::writers::WriterCandidateForm;
use crate::writers::input::WriterSource;
use crate::writers::model::UnknownSinkReason;
use crate::writers::registry::SinkRegistry;
use crate::writers::syntax::{
    enclosing_item_digest, function_name, function_signature_digest, normalized_node_digest,
};

pub(super) fn observe(
    source: &WriterSource,
    functions: &[Node<'_>],
    registry: &SinkRegistry,
    output: &mut ScanOutput<'_>,
) {
    for spec in registry.specs() {
        let Some(definition) = spec.definition() else {
            continue;
        };
        if definition.source() != source.path() {
            continue;
        }
        let matches: Vec<Node<'_>> = functions
            .iter()
            .copied()
            .filter(|function| definition_matches(*function, source.bytes(), definition.item()))
            .collect();
        if let [function] = matches.as_slice()
            && function_signature_digest(*function, source.bytes()) == definition.signature()
            && normalized_node_digest(*function, source.bytes()) == definition.implementation()
        {
            output.observed.insert(spec.id().clone());
            continue;
        }
        for function in matches {
            output.candidates.push(RawCandidate {
                path: source.path().clone(),
                start: function.start_byte(),
                end: function.end_byte(),
                enclosing_item: enclosing_item_digest(function, source.bytes()),
                normalized_call: normalized_node_digest(function, source.bytes()),
                candidate: spec.id().clone(),
                reason: UnknownSinkReason::DefinitionMismatch,
                form: WriterCandidateForm::WrapperDefinition,
            });
        }
    }
}

fn definition_matches(function: Node<'_>, bytes: &[u8], item: &str) -> bool {
    let Some(name) = function_name(function, bytes) else {
        return false;
    };
    definition_paths(function, bytes, &name)
        .iter()
        .any(|path| path == item)
}
