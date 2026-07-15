//! Tree event collection and enclosing-definition helpers.

use tree_sitter::Node;

use crate::rust::SourceRange;
use crate::writers::syntax::{canonical_identifier, normalized_path};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum EventKind {
    Call,
    Macro,
    MacroDefinition,
    Binding,
    Assignment,
    Return,
    StaticStorage,
}

#[derive(Clone, Copy)]
pub(super) struct Event<'tree> {
    pub(super) node: Node<'tree>,
    pub(super) kind: EventKind,
}

pub(super) fn collect_functions<'tree>(
    node: Node<'tree>,
    excluded: &[SourceRange],
    functions: &mut Vec<Node<'tree>>,
) {
    let mut pending = vec![node];
    while let Some(current) = pending.pop() {
        if is_excluded(current, excluded) {
            continue;
        }
        if current.kind() == "function_item" {
            functions.push(current);
        }
        for index in (0..current.named_child_count()).rev() {
            if let Some(child) = current.named_child(index) {
                pending.push(child);
            }
        }
    }
}

pub(super) fn collect_events<'tree>(
    node: Node<'tree>,
    include_function: bool,
    excluded: &[SourceRange],
    events: &mut Vec<Event<'tree>>,
) {
    let mut pending = vec![(node, include_function)];
    while let Some((current, admit_function)) = pending.pop() {
        if is_excluded(current, excluded) || (current.kind() == "function_item" && !admit_function)
        {
            continue;
        }
        let kind = match current.kind() {
            "call_expression" => Some(EventKind::Call),
            "macro_invocation" => Some(EventKind::Macro),
            "macro_definition" => Some(EventKind::MacroDefinition),
            "let_declaration" => Some(EventKind::Binding),
            "assignment_expression" => Some(EventKind::Assignment),
            "return_expression" => Some(EventKind::Return),
            "const_item" | "static_item" => Some(EventKind::StaticStorage),
            _ => None,
        };
        if let Some(kind) = kind {
            events.push(Event {
                node: current,
                kind,
            });
        }
        if matches!(current.kind(), "macro_invocation" | "macro_definition") {
            continue;
        }
        for index in (0..current.named_child_count()).rev() {
            if let Some(child) = current.named_child(index) {
                pending.push((child, false));
            }
        }
    }
}

pub(super) fn peel_generic(node: Node<'_>) -> Node<'_> {
    if node.kind() == "generic_function" {
        node.child_by_field_name("function").unwrap_or(node)
    } else {
        node
    }
}

pub(super) fn peel_callable(mut node: Node<'_>) -> Node<'_> {
    loop {
        node = peel_generic(node);
        let child = match node.kind() {
            "parenthesized_expression" | "reference_expression" => first_expression(node),
            "type_cast_expression" => node.child_by_field_name("value"),
            _ => None,
        };
        let Some(child) = child else {
            return node;
        };
        node = child;
    }
}

pub(super) fn first_expression(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).next()
}

pub(super) fn last_expression(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).last()
}

pub(super) fn terminal(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

pub(super) fn enclosing_impl_type(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "impl_item" {
            return parent
                .child_by_field_name("type")
                .and_then(|kind| normalized_path(kind, bytes));
        }
        current = parent.parent();
    }
    None
}

pub(super) fn definition_paths(node: Node<'_>, bytes: &[u8], name: &str) -> Vec<String> {
    if has_enclosing_function(node) {
        return Vec::new();
    }
    let mut components = enclosing_modules(node, bytes);
    if let Some(kind) = enclosing_impl_type(node, bytes) {
        components.push(kind);
    }
    components.push(name.to_owned());
    vec![components.join("::")]
}

fn has_enclosing_function(node: Node<'_>) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_item" {
            return true;
        }
        current = parent.parent();
    }
    false
}

pub(super) fn local_definition_path(node: Node<'_>, bytes: &[u8], path: &str) -> String {
    let mut components = enclosing_modules(node, bytes);
    components.push(path.to_owned());
    components.join("::")
}

fn enclosing_modules(node: Node<'_>, bytes: &[u8]) -> Vec<String> {
    let mut modules = Vec::new();
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "mod_item"
            && let Some(name) = parent.child_by_field_name("name")
            && let Some(name) = canonical_identifier(name, bytes)
        {
            modules.push(name);
        }
        current = parent.parent();
    }
    modules.reverse();
    modules
}

fn is_excluded(node: Node<'_>, excluded: &[SourceRange]) -> bool {
    excluded
        .iter()
        .any(|range| range.contains(node.start_byte(), node.end_byte()))
}
