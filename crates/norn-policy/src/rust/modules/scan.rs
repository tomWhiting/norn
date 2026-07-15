//! Tree-sitter node grouping and reference extraction.

use tree_sitter::Node;

use super::super::identifier;
use super::literal::decode_rust_string;
use super::model::SourceSpan;

pub(super) struct ScannedItem<'tree> {
    pub(super) node: Node<'tree>,
    pub(super) attributes: Vec<Node<'tree>>,
}

pub(super) struct ScannedContainer<'tree> {
    pub(super) inner_attributes: Vec<Node<'tree>>,
    pub(super) items: Vec<ScannedItem<'tree>>,
}

pub(super) fn scan_container(node: Node<'_>) -> Result<ScannedContainer<'_>, SourceSpan> {
    let mut inner_attributes = Vec::new();
    let mut attributes = Vec::new();
    let mut items = Vec::new();
    let mut saw_item = false;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "attribute_item" => attributes.push(child),
            "inner_attribute_item" if !saw_item && attributes.is_empty() => {
                inner_attributes.push(child);
            }
            "inner_attribute_item" => return Err(span(child)),
            "line_comment" | "block_comment" => {}
            _ => {
                saw_item = true;
                items.push(ScannedItem {
                    node: child,
                    attributes: std::mem::take(&mut attributes),
                });
            }
        }
    }
    if let Some(attribute) = attributes.first() {
        return Err(span(*attribute));
    }
    Ok(ScannedContainer {
        inner_attributes,
        items,
    })
}

pub(super) fn module_name(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    let name = node.child_by_field_name("name")?;
    let text = node_text(name, bytes)?;
    Some(identifier::canonical_text(text).to_owned())
}

pub(super) fn module_body(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("body")
}

pub(super) fn is_include(node: Node<'_>, bytes: &[u8]) -> bool {
    if node.kind() != "macro_invocation" {
        return false;
    }
    node.child_by_field_name("macro")
        .and_then(|name| node_text(name, bytes))
        .map(identifier::canonical_text)
        == Some("include")
}

pub(super) fn include_literal(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    let mut cursor = node.walk();
    let token_tree = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "token_tree")?;
    let text = node_text(token_tree, bytes)?.trim();
    let argument = text.strip_prefix('(')?.strip_suffix(')')?.trim();
    decode_rust_string(argument)
}

pub(super) fn span(node: Node<'_>) -> SourceSpan {
    SourceSpan::from_offsets(node.start_byte(), node.end_byte())
}

fn node_text<'bytes>(node: Node<'_>, bytes: &'bytes [u8]) -> Option<&'bytes str> {
    let Ok(text) = std::str::from_utf8(&bytes[node.byte_range()]) else {
        return None;
    };
    Some(text)
}
