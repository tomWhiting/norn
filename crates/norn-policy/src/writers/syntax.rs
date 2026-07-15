//! Rust syntax normalization shared by writer scanning and stable IDs.

use tree_sitter::Node;

use crate::digest::{Digest, digest_bytes};
use crate::rust::identifier;

pub(crate) fn node_text<'a>(node: Node<'_>, bytes: &'a [u8]) -> Option<&'a str> {
    let Ok(text) = std::str::from_utf8(bytes.get(node.byte_range())?) else {
        return None;
    };
    Some(text)
}

pub(crate) fn normalized_path(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    let mut segments = Vec::new();
    let mut current = node;
    loop {
        match current.kind() {
            "identifier" | "type_identifier" | "self" | "super" | "crate" => {
                segments.push(canonical_identifier(current, bytes)?);
                break;
            }
            "scoped_identifier" | "scoped_type_identifier" => {
                let name = current.child_by_field_name("name")?;
                segments.push(canonical_identifier(name, bytes)?);
                current = current.child_by_field_name("path")?;
            }
            _ => return None,
        }
    }
    segments.reverse();
    Some(segments.join("::"))
}

pub(crate) fn canonical_identifier(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    let text = node_text(node, bytes)?;
    Some(identifier::canonical_text(text).to_owned())
}

pub(crate) fn normalized_node_digest(node: Node<'_>, bytes: &[u8]) -> Digest {
    let mut normalized = Vec::new();
    append_normalized(node, bytes, &mut normalized);
    digest_bytes(&normalized)
}

pub(crate) fn function_signature_digest(node: Node<'_>, bytes: &[u8]) -> Digest {
    let body = node.child_by_field_name("body").map(|child| child.id());
    let mut normalized = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if Some(child.id()) != body {
            append_normalized(child, bytes, &mut normalized);
        }
    }
    digest_bytes(&normalized)
}

pub(crate) fn enclosing_item_digest(node: Node<'_>, bytes: &[u8]) -> Digest {
    let mut components = Vec::new();
    let mut child = node;
    let mut current = node.parent();
    while let Some(parent) = current {
        if let Some(component) = item_component(parent, bytes) {
            components.push(component);
        }
        if let Some(component) = lexical_scope_component(parent, child, bytes) {
            components.push(component);
        }
        child = parent;
        current = parent.parent();
    }
    components.reverse();
    if components.is_empty() {
        components.push("module".to_owned());
    }
    let mut framed = Vec::new();
    for component in components {
        append_framed(&mut framed, component.as_bytes());
    }
    digest_bytes(&framed)
}

pub(crate) fn binding_name(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    let mut current = node;
    loop {
        match current.kind() {
            "identifier" => return canonical_identifier(current, bytes),
            "reference_pattern" | "mut_pattern" | "captured_pattern" => {
                current = single_named_child(current)?;
            }
            _ => return None,
        }
    }
}

pub(crate) fn identifier_name(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    (node.kind() == "identifier")
        .then(|| canonical_identifier(node, bytes))
        .flatten()
}

pub(crate) fn macro_identifier_nodes<'tree>(node: Node<'tree>, bytes: &[u8]) -> Vec<Node<'tree>> {
    let mut identifiers = Vec::new();
    let mut pending = vec![node];
    while let Some(current) = pending.pop() {
        if current.kind() == "identifier" {
            if node_text(current, bytes).is_some() {
                identifiers.push(current);
            }
            continue;
        }
        if is_literal_node(current) {
            continue;
        }
        for index in (0..current.named_child_count()).rev() {
            if let Some(child) = current.named_child(index) {
                pending.push(child);
            }
        }
    }
    identifiers.sort_by_key(|identifier| (identifier.start_byte(), identifier.end_byte()));
    identifiers
}

pub(crate) fn enclosing_is_generic(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_item" {
            return parent.child_by_field_name("type_parameters").is_some();
        }
        current = parent.parent();
    }
    false
}

pub(crate) fn function_name(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    node.child_by_field_name("name")
        .and_then(|name| canonical_identifier(name, bytes))
}

fn append_normalized(node: Node<'_>, bytes: &[u8], output: &mut Vec<u8>) {
    let mut pending = vec![node];
    while let Some(current) = pending.pop() {
        if matches!(current.kind(), "line_comment" | "block_comment") {
            continue;
        }
        if is_literal_node(current) || current.child_count() == 0 {
            append_token(current, bytes, output);
            continue;
        }
        for index in (0..current.child_count()).rev() {
            if let Some(child) = current.child(index) {
                pending.push(child);
            }
        }
    }
}

fn append_token(node: Node<'_>, bytes: &[u8], output: &mut Vec<u8>) {
    append_framed(output, node.kind().as_bytes());
    if is_identifier_node(node) {
        if let Some(identifier) = canonical_identifier(node, bytes) {
            append_framed(output, identifier.as_bytes());
        } else {
            append_framed(output, &[]);
        }
    } else if let Some(text) = bytes.get(node.byte_range()) {
        append_framed(output, text);
    } else {
        append_framed(output, &[]);
    }
}

fn item_component(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "mod_item" | "function_item" | "trait_item" | "const_item" | "static_item"
        | "type_item" | "macro_definition" => named_component(node, bytes),
        "impl_item" => Some(header_component("impl", node, bytes, "body")),
        "closure_expression" => Some(header_component("closure", node, bytes, "body")),
        _ => None,
    }
}

fn named_component(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    let name = node.child_by_field_name("name")?;
    let name = canonical_identifier(name, bytes)?;
    Some(format!("{}:{name}", node.kind()))
}

fn is_identifier_node(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "identifier" | "type_identifier" | "field_identifier"
    )
}

fn is_literal_node(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "string_literal" | "raw_string_literal" | "char_literal"
    )
}

fn append_framed(output: &mut Vec<u8>, value: &[u8]) {
    let length = value.len().to_be_bytes();
    output.extend_from_slice(&[0_u8; 16][length.len()..]);
    output.extend_from_slice(&length);
    output.extend_from_slice(value);
}

fn header_component(prefix: &str, node: Node<'_>, bytes: &[u8], body_field: &str) -> String {
    let body = node.child_by_field_name(body_field).map(|child| child.id());
    let mut normalized = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if Some(child.id()) != body {
            append_normalized(child, bytes, &mut normalized);
        }
    }
    format!("{prefix}:{}", digest_bytes(&normalized))
}

fn lexical_scope_component(node: Node<'_>, child: Node<'_>, bytes: &[u8]) -> Option<String> {
    match node.kind() {
        "let_declaration" => Some(fields_component("let", node, bytes, &["pattern", "type"])),
        "match_arm" => Some(header_component("arm", node, bytes, "value")),
        "if_expression" => Some(format!(
            "if:{}:{}",
            field_digest(node, "condition", bytes),
            branch_name(node, child)
        )),
        "for_expression" => Some(fields_component("for", node, bytes, &["pattern", "value"])),
        "while_expression" => Some(fields_component("while", node, bytes, &["condition"])),
        "call_expression" if child.kind() == "arguments" => {
            Some(fields_component("call_scope", node, bytes, &["function"]))
        }
        "async_block" | "unsafe_block" | "try_block" => Some(node.kind().to_owned()),
        _ => None,
    }
}

fn fields_component(prefix: &str, node: Node<'_>, bytes: &[u8], fields: &[&str]) -> String {
    let mut normalized = Vec::new();
    for field in fields {
        if let Some(child) = node.child_by_field_name(field) {
            append_normalized(child, bytes, &mut normalized);
        }
    }
    format!("{prefix}:{}", digest_bytes(&normalized))
}

fn field_digest(node: Node<'_>, field: &str, bytes: &[u8]) -> Digest {
    node.child_by_field_name(field).map_or_else(
        || digest_bytes(&[]),
        |child| normalized_node_digest(child, bytes),
    )
}

fn branch_name(node: Node<'_>, child: Node<'_>) -> &'static str {
    if node
        .child_by_field_name("consequence")
        .is_some_and(|branch| branch.id() == child.id())
    {
        "consequence"
    } else if node
        .child_by_field_name("alternative")
        .is_some_and(|branch| branch.id() == child.id())
    {
        "alternative"
    } else {
        "condition"
    }
}

fn single_named_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    let mut children = node.named_children(&mut cursor);
    let first = children.next()?;
    children.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use crate::rust::RustSource;

    use super::normalized_node_digest;

    #[test]
    fn normalization_uses_heap_traversal_for_deep_syntax() -> Result<(), Box<dyn Error>> {
        const DEPTH: usize = 20_000;

        let mut source = String::with_capacity(DEPTH * 2 + 64);
        source.push_str("fn run() { std::fs::write(");
        source.extend(std::iter::repeat_n('(', DEPTH));
        source.push_str("\"alpha\"");
        source.extend(std::iter::repeat_n(')', DEPTH));
        source.push_str(", b\"x\"); }");

        let parsed = RustSource::parse(source.into_bytes())?;
        let digest = normalized_node_digest(parsed.root_node(), parsed.bytes());
        assert_ne!(digest, crate::digest::digest_bytes(&[]));
        Ok(())
    }
}
