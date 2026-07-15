//! Stable debt occurrence identity and structural normalization.

use std::collections::BTreeMap;
use std::ops::Range;

use tree_sitter::Node;

use crate::debt::model::{DebtConstructKind, DebtOccurrence, DebtScanError, DebtTargetContext};
use crate::digest::{Digest, digest_bytes};
use crate::finding::ByteSpan;
use crate::path::RepositoryPath;
use crate::version::ANALYZER_VERSION;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DebtDraft {
    pub(super) construct: DebtConstructKind,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) item_identity: Digest,
    pub(super) syntax_digest: Digest,
    pub(super) scope_digest: Digest,
}

pub(super) fn draft_for_node(
    construct: DebtConstructKind,
    node: Node<'_>,
    context: Node<'_>,
    bytes: &[u8],
) -> Result<DebtDraft, DebtScanError> {
    draft_for_span(
        construct,
        node.start_byte()..node.end_byte(),
        node,
        context,
        &normalized_node(node, bytes),
        bytes,
    )
}

pub(super) fn draft_for_span(
    construct: DebtConstructKind,
    span: Range<usize>,
    node: Node<'_>,
    context: Node<'_>,
    normalized_syntax: &[u8],
    bytes: &[u8],
) -> Result<DebtDraft, DebtScanError> {
    let scope_digest = scope_identity(node)?;
    let scope_digest = if node == context {
        scope_digest
    } else {
        let mut encoded = Vec::new();
        append_field(&mut encoded, scope_digest.as_bytes());
        append_field(&mut encoded, &normalized_node(context, bytes));
        digest_bytes(&encoded)
    };
    Ok(DebtDraft {
        construct,
        start: span.start,
        end: span.end,
        item_identity: item_identity(context, bytes),
        syntax_digest: digest_bytes(normalized_syntax),
        scope_digest,
    })
}

pub(super) fn finalize(
    path: &RepositoryPath,
    target: &DebtTargetContext,
    mut drafts: Vec<DebtDraft>,
) -> Result<Vec<DebtOccurrence>, DebtScanError> {
    drafts.sort_by_key(|draft| (draft.start, draft.end, draft.construct));
    let mut ordinals = BTreeMap::new();
    let mut occurrences = Vec::with_capacity(drafts.len());
    for draft in drafts {
        let key = (
            draft.construct,
            draft.item_identity,
            draft.syntax_digest,
            draft.scope_digest,
        );
        let ordinal = ordinals.entry(key).or_insert(0_u32);
        let current_ordinal = *ordinal;
        *ordinal = ordinal.checked_add(1).ok_or(DebtScanError::Ordinal)?;
        let Ok(start) = u64::try_from(draft.start) else {
            return Err(DebtScanError::Span {
                offset: draft.start,
            });
        };
        let Ok(end) = u64::try_from(draft.end) else {
            return Err(DebtScanError::Span { offset: draft.end });
        };
        let Ok(span) = ByteSpan::new(start, end) else {
            return Err(DebtScanError::Span {
                offset: draft.start,
            });
        };
        let fingerprint = fingerprint(path, target, &draft, current_ordinal);
        occurrences.push(DebtOccurrence {
            path: path.clone(),
            target: target.clone(),
            construct: draft.construct,
            span,
            item_identity: draft.item_identity,
            syntax_digest: draft.syntax_digest,
            scope_digest: draft.scope_digest,
            ordinal: current_ordinal,
            fingerprint,
        });
    }
    Ok(occurrences)
}

fn fingerprint(
    path: &RepositoryPath,
    target: &DebtTargetContext,
    draft: &DebtDraft,
    ordinal: u32,
) -> Digest {
    let mut encoded = Vec::new();
    append_field(&mut encoded, ANALYZER_VERSION.as_bytes());
    append_field(&mut encoded, path.as_str().as_bytes());
    append_field(&mut encoded, target.kind().as_str().as_bytes());
    append_field(&mut encoded, target.identity().as_bytes());
    append_field(&mut encoded, draft.construct.as_str().as_bytes());
    append_field(&mut encoded, draft.item_identity.as_bytes());
    append_field(&mut encoded, draft.syntax_digest.as_bytes());
    append_field(&mut encoded, draft.scope_digest.as_bytes());
    append_field(&mut encoded, &ordinal.to_be_bytes());
    digest_bytes(&encoded)
}

fn item_identity(node: Node<'_>, bytes: &[u8]) -> Digest {
    let mut items = Vec::new();
    let mut cursor = Some(node);
    while let Some(current) = cursor {
        if is_identity_item(current.kind()) {
            items.push(identity_component(current, bytes));
        }
        cursor = current.parent();
    }
    items.reverse();
    let mut encoded = Vec::new();
    append_field(&mut encoded, b"crate");
    for item in items {
        append_field(&mut encoded, &item);
    }
    digest_bytes(&encoded)
}

fn identity_component(node: Node<'_>, bytes: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    append_field(&mut encoded, node.kind().as_bytes());
    if node.kind() == "impl_item" {
        let body_start = node
            .child_by_field_name("body")
            .map_or(node.end_byte(), |body| body.start_byte());
        append_field(
            &mut encoded,
            &normalize_fragment(&bytes[node.start_byte()..body_start]),
        );
    } else if let Some(name) = node.child_by_field_name("name") {
        append_field(&mut encoded, &normalized_node(name, bytes));
    } else if node.kind() == "foreign_mod_item" {
        append_field(&mut encoded, &normalized_header(node, bytes));
    }
    encoded
}

fn normalized_header(node: Node<'_>, bytes: &[u8]) -> Vec<u8> {
    let body_start = node
        .child_by_field_name("body")
        .map_or(node.end_byte(), |body| body.start_byte());
    normalize_fragment(&bytes[node.start_byte()..body_start])
}

fn scope_identity(node: Node<'_>) -> Result<Digest, DebtScanError> {
    let mut scope = node.parent();
    while let Some(candidate) = scope {
        if is_identity_item(candidate.kind()) {
            scope = None;
            break;
        }
        if is_lexical_scope(candidate.kind()) {
            break;
        }
        scope = candidate.parent();
    }
    let mut edges = Vec::new();
    let Some(mut child) = scope else {
        return Ok(digest_bytes(b"item_scope"));
    };
    while let Some(parent) = child.parent() {
        let (named_index, field) = named_child_position(parent, child)?;
        let mut edge = Vec::new();
        append_field(&mut edge, parent.kind().as_bytes());
        append_field(&mut edge, field.unwrap_or("-").as_bytes());
        append_field(&mut edge, &named_index.to_be_bytes());
        edges.push(edge);
        if is_identity_item(parent.kind()) {
            break;
        }
        child = parent;
    }
    edges.reverse();
    let mut encoded = Vec::new();
    for edge in edges {
        append_field(&mut encoded, &edge);
    }
    Ok(digest_bytes(&encoded))
}

fn named_child_position(
    parent: Node<'_>,
    child: Node<'_>,
) -> Result<(u64, Option<&'static str>), DebtScanError> {
    let mut cursor = parent.walk();
    for (index, candidate) in parent.named_children(&mut cursor).enumerate() {
        if candidate == child {
            let Ok(field_index) = u32::try_from(index) else {
                return Err(DebtScanError::Span {
                    offset: child.start_byte(),
                });
            };
            let Ok(stable_index) = u64::try_from(index) else {
                return Err(DebtScanError::Span {
                    offset: child.start_byte(),
                });
            };
            return Ok((stable_index, parent.field_name_for_named_child(field_index)));
        }
    }
    Err(DebtScanError::Span {
        offset: child.start_byte(),
    })
}

pub(super) fn normalized_node(node: Node<'_>, bytes: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    append_normalized_node(node, bytes, &mut encoded);
    encoded
}

fn append_normalized_node(node: Node<'_>, bytes: &[u8], output: &mut Vec<u8>) {
    if matches!(node.kind(), "line_comment" | "block_comment") {
        return;
    }
    if node.child_count() == 0 {
        append_field(output, node.kind().as_bytes());
        append_field(output, &normalize_fragment(&bytes[node.byte_range()]));
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        append_normalized_node(child, bytes, output);
    }
}

fn normalize_fragment(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\r' && bytes.get(cursor + 1) == Some(&b'\n') {
            normalized.push(b'\n');
            cursor += 2;
        } else if !bytes[cursor].is_ascii_whitespace() {
            normalized.push(bytes[cursor]);
            cursor += 1;
        } else {
            cursor += 1;
        }
    }
    normalized
}

fn is_identity_item(kind: &str) -> bool {
    matches!(
        kind,
        "mod_item"
            | "function_item"
            | "function_signature_item"
            | "impl_item"
            | "trait_item"
            | "struct_item"
            | "enum_item"
            | "union_item"
            | "type_item"
            | "const_item"
            | "static_item"
            | "foreign_mod_item"
            | "macro_definition"
    )
}

fn is_lexical_scope(kind: &str) -> bool {
    matches!(
        kind,
        "block"
            | "async_block"
            | "const_block"
            | "closure_expression"
            | "match_arm"
            | "declaration_list"
            | "source_file"
    )
}

fn append_field(output: &mut Vec<u8>, value: &[u8]) {
    let length = value.len().to_be_bytes();
    output.extend_from_slice(&[0_u8; 16][length.len()..]);
    output.extend_from_slice(&length);
    output.extend_from_slice(value);
}
