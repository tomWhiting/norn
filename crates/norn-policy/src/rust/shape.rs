//! Declaration-only production `mod.rs` validation.

use serde::Serialize;
use tree_sitter::Node;

use super::syntax::{RustSource, RustSourceError};

/// Prohibited production top-level form in a `mod.rs` file.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleShapeKind {
    /// An external module declaration contains an inline body.
    InlineModule,
    /// A use declaration has no explicit visibility.
    PrivateUse,
    /// A different logic-bearing or named form appears at top level.
    OtherItem,
    /// An attribute is not attached to a permitted item.
    UnattachedAttribute,
}

/// One deterministic module-shape violation.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ModuleShapeViolation {
    /// Stable prohibited form.
    pub kind: ModuleShapeKind,
    /// Inclusive start byte.
    pub start: usize,
    /// Exclusive end byte.
    pub end: usize,
}

/// Validate the top level of production `mod.rs` bytes.
///
/// # Errors
///
/// Rust syntax and cfg failures are returned before structural findings.
pub fn module_shape(bytes: &[u8]) -> Result<Vec<ModuleShapeViolation>, RustSourceError> {
    let source = RustSource::parse(bytes.to_vec())?;
    let excluded = source.test_only_ranges()?;
    let mut cursor = source.root_node().walk();
    let children: Vec<Node<'_>> = source.root_node().named_children(&mut cursor).collect();
    let mut attributes = Vec::new();
    let mut violations = Vec::new();
    for child in children {
        if excluded
            .iter()
            .any(|range| range.contains(child.start_byte(), child.end_byte()))
        {
            attributes.clear();
            continue;
        }
        match child.kind() {
            "attribute_item" => attributes.push(child),
            "line_comment" | "block_comment" => {}
            "mod_item" if child.child_by_field_name("body").is_none() => attributes.clear(),
            "mod_item" => {
                violations.push(violation(ModuleShapeKind::InlineModule, child));
                attributes.clear();
            }
            "use_declaration" if has_visible_reexport(child, source.bytes()) => {
                attributes.clear();
            }
            "use_declaration" => {
                violations.push(violation(ModuleShapeKind::PrivateUse, child));
                attributes.clear();
            }
            _ => {
                violations.push(violation(ModuleShapeKind::OtherItem, child));
                attributes.clear();
            }
        }
    }
    if let Some(attribute) = attributes.first().copied() {
        violations.push(violation(ModuleShapeKind::UnattachedAttribute, attribute));
    }
    violations.sort_unstable();
    Ok(violations)
}

fn has_visible_reexport(node: Node<'_>, bytes: &[u8]) -> bool {
    let mut cursor = node.walk();
    let Some(visibility) = node
        .named_children(&mut cursor)
        .find(|child| child.kind() == "visibility_modifier")
    else {
        return false;
    };
    let compact = bytes[visibility.byte_range()]
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect::<Vec<_>>();
    compact == b"pub"
        || compact == b"pub(crate)"
        || compact == b"pub(super)"
        || compact == b"pub(incrate)"
        || compact == b"pub(insuper)"
        || compact.starts_with(b"pub(incrate::")
        || compact.starts_with(b"pub(insuper::")
}

fn violation(kind: ModuleShapeKind, node: Node<'_>) -> ModuleShapeViolation {
    ModuleShapeViolation {
        kind,
        start: node.start_byte(),
        end: node.end_byte(),
    }
}
