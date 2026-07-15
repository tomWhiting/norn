//! Rust syntax traversal for prohibited-debt occurrences.

use std::collections::BTreeSet;

use tree_sitter::Node;

use crate::debt::fingerprint::{
    DebtDraft, draft_for_node, draft_for_span, finalize, normalized_node,
};
use crate::debt::meta::analyze_attribute;
use crate::debt::model::{DebtConstructKind, DebtOccurrence, DebtScanError, DebtTargetContext};
use crate::path::RepositoryPath;
use crate::rust::{RustSource, identifier};

const TASK_MARKER: &[u8] = concat!("TO", "DO").as_bytes();
const REPAIR_MARKER: &[u8] = concat!("FIX", "ME").as_bytes();
const SHORTCUT_MARKER: &[u8] = concat!("HA", "CK").as_bytes();

/// Scan one parsed Rust source in one Cargo target context.
///
/// Findings are returned in source order. Their full fingerprints exclude byte
/// positions, so whitespace-only line shifts do not change identity.
///
/// # Errors
///
/// Fails closed for invalid Rust, unsupported relevant attribute metadata,
/// missing structural fields in recognized syntax, or unrepresentable spans.
pub fn scan_rust_debt(
    path: &RepositoryPath,
    target: &DebtTargetContext,
    bytes: &[u8],
) -> Result<Vec<DebtOccurrence>, DebtScanError> {
    let source = RustSource::parse(bytes.to_vec())?;
    let mut scanner = Scanner {
        bytes: source.bytes(),
        drafts: Vec::new(),
        binding_spans: BTreeSet::new(),
    };
    scanner.visit(source.root_node())?;
    scanner.scan_markers(source.root_node())?;
    finalize(path, target, scanner.drafts)
}

struct Scanner<'a> {
    bytes: &'a [u8],
    drafts: Vec<DebtDraft>,
    binding_spans: BTreeSet<(usize, usize)>,
}

impl Scanner<'_> {
    fn visit(&mut self, node: Node<'_>) -> Result<(), DebtScanError> {
        match node.kind() {
            "attribute_item" | "inner_attribute_item" => self.scan_attribute(node)?,
            "call_expression" => self.scan_call(node)?,
            "macro_invocation" => self.scan_macro(node)?,
            "token_tree" if !has_token_tree_ancestor(node) => {
                self.scan_token_tree(node)?;
            }
            "parameter" | "let_declaration" | "let_condition" | "for_expression" | "match_arm" => {
                self.scan_pattern_field(node)?;
            }
            "closure_parameters" => self.scan_closure_parameters(node)?,
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.visit(child)?;
        }
        Ok(())
    }

    fn scan_attribute(&mut self, node: Node<'_>) -> Result<(), DebtScanError> {
        let Ok(text) = std::str::from_utf8(&self.bytes[node.byte_range()]) else {
            return Err(DebtScanError::UnsupportedSyntax {
                offset: node.start_byte(),
            });
        };
        let context = attribute_context(node)?;
        for debt in analyze_attribute(text)? {
            let start = node
                .start_byte()
                .checked_add(debt.start)
                .ok_or(DebtScanError::Span {
                    offset: node.start_byte(),
                })?;
            let end = node
                .start_byte()
                .checked_add(debt.end)
                .ok_or(DebtScanError::Span {
                    offset: node.start_byte(),
                })?;
            self.drafts.push(draft_for_span(
                debt.construct,
                start..end,
                node,
                context,
                &debt.normalized,
                self.bytes,
            )?);
        }
        Ok(())
    }

    fn scan_call(&mut self, node: Node<'_>) -> Result<(), DebtScanError> {
        let Some(function) = node.child_by_field_name("function") else {
            return Err(DebtScanError::UnsupportedSyntax {
                offset: node.start_byte(),
            });
        };
        let callable = callable_name(function)?;
        let Some(callable) = callable else {
            return Ok(());
        };
        if let Some(construct) = prohibited_call(identifier::canonical_bytes(
            &self.bytes[callable.byte_range()],
        )) {
            self.drafts
                .push(draft_for_node(construct, node, node, self.bytes)?);
        }
        Ok(())
    }

    fn scan_macro(&mut self, node: Node<'_>) -> Result<(), DebtScanError> {
        let Some(name) = node.child_by_field_name("macro") else {
            return Err(DebtScanError::UnsupportedSyntax {
                offset: node.start_byte(),
            });
        };
        let Ok(raw) = std::str::from_utf8(&self.bytes[name.byte_range()]) else {
            return Err(DebtScanError::UnsupportedSyntax {
                offset: name.start_byte(),
            });
        };
        let leaf = raw.rsplit("::").next().unwrap_or(raw);
        let leaf = identifier::canonical_text(leaf);
        let construct = match leaf {
            "panic" => Some(DebtConstructKind::PanicMacro),
            "todo" => Some(DebtConstructKind::TodoMacro),
            "unimplemented" => Some(DebtConstructKind::UnimplementedMacro),
            "unreachable" => Some(DebtConstructKind::UnreachableMacro),
            _ => None,
        };
        if let Some(construct) = construct {
            self.drafts
                .push(draft_for_node(construct, node, node, self.bytes)?);
        }
        Ok(())
    }

    fn scan_token_tree(&mut self, node: Node<'_>) -> Result<(), DebtScanError> {
        let mut leaves = Vec::new();
        collect_token_leaves(node, &mut leaves);
        for window in leaves.windows(3) {
            let separator = &self.bytes[window[0].byte_range()];
            if (separator != b"." && separator != b"::")
                || &self.bytes[window[2].byte_range()] != b"("
            {
                continue;
            }
            let construct = prohibited_call(identifier::canonical_bytes(
                &self.bytes[window[1].byte_range()],
            ));
            let Some(construct) = construct else {
                continue;
            };
            let Some(arguments) = window[2].parent() else {
                return Err(DebtScanError::UnsupportedSyntax {
                    offset: window[2].start_byte(),
                });
            };
            if arguments.kind() != "token_tree" {
                return Err(DebtScanError::UnsupportedSyntax {
                    offset: arguments.start_byte(),
                });
            }
            let mut normalized = normalized_node(window[1], self.bytes);
            normalized.extend_from_slice(&normalized_node(arguments, self.bytes));
            self.drafts.push(draft_for_span(
                construct,
                window[0].start_byte()..arguments.end_byte(),
                window[1],
                window[1],
                &normalized,
                self.bytes,
            )?);
        }
        Ok(())
    }

    fn scan_pattern_field(&mut self, node: Node<'_>) -> Result<(), DebtScanError> {
        let Some(pattern) = node.child_by_field_name("pattern") else {
            return Err(DebtScanError::UnsupportedSyntax {
                offset: node.start_byte(),
            });
        };
        self.collect_bindings(pattern)
    }

    fn scan_closure_parameters(&mut self, node: Node<'_>) -> Result<(), DebtScanError> {
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if child.kind() != "parameter" && child.kind() != "attribute_item" {
                self.collect_bindings(child)?;
            }
        }
        Ok(())
    }

    fn collect_bindings(&mut self, node: Node<'_>) -> Result<(), DebtScanError> {
        if node.kind() == "field_pattern" {
            if let Some(pattern) = node.child_by_field_name("pattern") {
                return self.collect_bindings(pattern);
            }
            if let Some(name) = node.child_by_field_name("name") {
                return self.record_binding(name);
            }
            return Err(DebtScanError::UnsupportedSyntax {
                offset: node.start_byte(),
            });
        }
        if node.kind() == "identifier" || node.kind() == "shorthand_field_identifier" {
            return self.record_binding(node);
        }
        let type_node = node.child_by_field_name("type");
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if type_node != Some(child) {
                self.collect_bindings(child)?;
            }
        }
        Ok(())
    }

    fn record_binding(&mut self, node: Node<'_>) -> Result<(), DebtScanError> {
        let name = identifier::canonical_bytes(&self.bytes[node.byte_range()]);
        if name.len() <= 1 || name.first() != Some(&b'_') {
            return Ok(());
        }
        let span = (node.start_byte(), node.end_byte());
        if self.binding_spans.insert(span) {
            self.drafts.push(draft_for_node(
                DebtConstructKind::UnderscoreBinding,
                node,
                node,
                self.bytes,
            )?);
        }
        Ok(())
    }

    fn scan_markers(&mut self, root: Node<'_>) -> Result<(), DebtScanError> {
        let markers = [
            (TASK_MARKER, DebtConstructKind::TodoMarker),
            (REPAIR_MARKER, DebtConstructKind::FixmeMarker),
            (SHORTCUT_MARKER, DebtConstructKind::HackMarker),
        ];
        for (marker, construct) in markers {
            let mut cursor = 0;
            while let Some(relative) = find_bytes(&self.bytes[cursor..], marker) {
                let start = cursor + relative;
                let end = start + marker.len();
                let node = root
                    .descendant_for_byte_range(start, end)
                    .ok_or(DebtScanError::UnsupportedSyntax { offset: start })?;
                let mut normalized = Vec::new();
                normalized.extend_from_slice(node.kind().as_bytes());
                normalized.push(0);
                normalized.extend_from_slice(marker);
                self.drafts.push(draft_for_span(
                    construct,
                    start..end,
                    node,
                    node,
                    &normalized,
                    self.bytes,
                )?);
                cursor = end;
            }
        }
        Ok(())
    }
}

fn collect_token_leaves<'tree>(node: Node<'tree>, leaves: &mut Vec<Node<'tree>>) {
    if is_opaque_token(node.kind()) {
        return;
    }
    if node.child_count() == 0 {
        leaves.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_token_leaves(child, leaves);
    }
}

fn has_token_tree_ancestor(node: Node<'_>) -> bool {
    let mut parent = node.parent();
    while let Some(ancestor) = parent {
        if ancestor.kind() == "token_tree" {
            return true;
        }
        parent = ancestor.parent();
    }
    false
}

fn is_opaque_token(kind: &str) -> bool {
    matches!(kind, "line_comment" | "block_comment") || kind.ends_with("_literal")
}

fn attribute_context(node: Node<'_>) -> Result<Node<'_>, DebtScanError> {
    if node.kind() == "inner_attribute_item" {
        return Ok(node);
    }
    let mut candidate = node.next_named_sibling();
    while let Some(sibling) = candidate {
        if !matches!(
            sibling.kind(),
            "attribute_item" | "line_comment" | "block_comment"
        ) {
            return Ok(sibling);
        }
        candidate = sibling.next_named_sibling();
    }
    Err(DebtScanError::UnsupportedSyntax {
        offset: node.start_byte(),
    })
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn callable_name(node: Node<'_>) -> Result<Option<Node<'_>>, DebtScanError> {
    let field = match node.kind() {
        "field_expression" => "field",
        "scoped_identifier" | "scoped_type_identifier" => "name",
        "generic_function" => {
            let callable =
                node.child_by_field_name("function")
                    .ok_or(DebtScanError::UnsupportedSyntax {
                        offset: node.start_byte(),
                    })?;
            return callable_name(callable);
        }
        _ => return Ok(None),
    };
    node.child_by_field_name(field)
        .map(Some)
        .ok_or(DebtScanError::UnsupportedSyntax {
            offset: node.start_byte(),
        })
}

fn prohibited_call(identifier: &[u8]) -> Option<DebtConstructKind> {
    match identifier {
        b"unwrap" => Some(DebtConstructKind::UnwrapCall),
        b"unwrap_err" => Some(DebtConstructKind::UnwrapErrCall),
        b"expect" => Some(DebtConstructKind::ExpectCall),
        b"expect_err" => Some(DebtConstructKind::ExpectErrCall),
        _ => None,
    }
}
