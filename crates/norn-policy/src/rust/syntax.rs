//! Parsed Rust sources and production-only conditional ranges.

use std::sync::Arc;

use serde::Serialize;
use thiserror::Error;
use tree_sitter::{Node, Parser, Tree};

mod meta;

use super::{CfgError, CfgTruth};
use meta::meta_truth;

/// A half-open byte range proved absent from every production configuration.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SourceRange {
    start: usize,
    end: usize,
}

impl SourceRange {
    /// Return the inclusive starting byte.
    #[must_use]
    pub const fn start(self) -> usize {
        self.start
    }

    /// Return the exclusive ending byte.
    #[must_use]
    pub const fn end(self) -> usize {
        self.end
    }

    pub(crate) const fn contains(self, start: usize, end: usize) -> bool {
        self.start <= start && end <= self.end
    }

    pub(crate) const fn overlaps(self, start: usize, end: usize) -> bool {
        self.start < end && start < self.end
    }
}

/// One owned, successfully parsed Rust source.
pub struct RustSource {
    bytes: Arc<[u8]>,
    tree: Tree,
}

impl RustSource {
    /// Parse owned Rust source without consulting the filesystem.
    ///
    /// # Errors
    ///
    /// Rejects non-UTF-8 bytes, parser initialization failures, unavailable
    /// parse results, and syntax trees containing an error node.
    pub fn parse(bytes: impl Into<Arc<[u8]>>) -> Result<Self, RustSourceError> {
        let bytes = bytes.into();
        std::str::from_utf8(&bytes).map_err(RustSourceError::Utf8)?;
        let mut parser = Parser::new();
        let language = tree_sitter_rust::LANGUAGE.into();
        parser
            .set_language(&language)
            .map_err(RustSourceError::Language)?;
        let tree = parser.parse(&bytes, None).ok_or(RustSourceError::Parse)?;
        if tree.root_node().has_error() {
            return Err(RustSourceError::Syntax);
        }
        Ok(Self { bytes, tree })
    }

    /// Borrow the exact source bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Compute the merged ranges proved test-only in production.
    ///
    /// # Errors
    ///
    /// Returns a closed failure for malformed or unsupported `cfg` and
    /// `cfg_attr` metadata rather than treating it as excluded.
    pub fn test_only_ranges(&self) -> Result<Vec<SourceRange>, RustSourceError> {
        let mut ranges = Vec::new();
        collect_test_only(
            self.tree.root_node(),
            &self.bytes,
            &mut ranges,
            CfgTruth::True,
        )?;
        ranges.sort_unstable();
        Ok(merge_ranges(ranges))
    }

    pub(crate) fn root_node(&self) -> Node<'_> {
        self.tree.root_node()
    }
}

/// Rust parsing and production-range failures.
#[derive(Debug, Error)]
pub enum RustSourceError {
    /// Rust source must be valid UTF-8.
    #[error("Rust source is not valid UTF-8")]
    Utf8(#[source] std::str::Utf8Error),
    /// The pinned Rust grammar could not be installed.
    #[error("pinned Rust grammar could not be installed")]
    Language(#[source] tree_sitter::LanguageError),
    /// Tree-sitter returned no parse tree.
    #[error("pinned Rust parser returned no tree")]
    Parse,
    /// The resulting tree contains a syntax error.
    #[error("Rust source contains a syntax error")]
    Syntax,
    /// A cfg expression was invalid or unsupported.
    #[error("Rust cfg expression is invalid")]
    Cfg(#[from] CfgError),
    /// A cfg attribute had an unsupported structural form.
    #[error("Rust cfg attribute is unsupported at byte {offset}")]
    Attribute {
        /// Byte offset of the attribute.
        offset: usize,
    },
}

fn collect_test_only(
    node: Node<'_>,
    bytes: &[u8],
    ranges: &mut Vec<SourceRange>,
    inherited: CfgTruth,
) -> Result<(), RustSourceError> {
    let mut pending = vec![(node, inherited)];
    while let Some((current, inherited)) = pending.pop() {
        let inherited = combine(inherited, container_inner_truth(current, bytes)?);
        if inherited == CfgTruth::False {
            ranges.push(SourceRange {
                start: current.start_byte(),
                end: current.end_byte(),
            });
            continue;
        }
        let mut cursor = current.walk();
        let mut attributes = Vec::new();
        let mut descendants = Vec::new();
        for child in current.named_children(&mut cursor) {
            match child.kind() {
                "attribute_item" => attributes.push(child),
                "line_comment" | "block_comment" | "inner_attribute_item" => {}
                _ => {
                    let local = attributes_truth(&attributes, bytes)?;
                    let truth = combine(inherited, local);
                    if truth == CfgTruth::False {
                        let start = attributes
                            .first()
                            .map_or(child.start_byte(), tree_sitter::Node::start_byte);
                        ranges.push(SourceRange {
                            start,
                            end: child.end_byte(),
                        });
                    } else {
                        descendants.push((child, truth));
                    }
                    attributes.clear();
                }
            }
        }
        if let Some(attribute) = attributes.first() {
            return Err(RustSourceError::Attribute {
                offset: attribute.start_byte(),
            });
        }
        pending.extend(descendants.into_iter().rev());
    }
    Ok(())
}

fn attributes_truth(attributes: &[Node<'_>], bytes: &[u8]) -> Result<CfgTruth, RustSourceError> {
    let mut truth = CfgTruth::True;
    for attribute in attributes {
        truth = combine(truth, attribute_truth(*attribute, bytes)?);
    }
    Ok(truth)
}

fn container_inner_truth(node: Node<'_>, bytes: &[u8]) -> Result<CfgTruth, RustSourceError> {
    let mut truth = direct_inner_truth(node, bytes)?;
    if node.kind() == "mod_item"
        && let Some(body) = node.child_by_field_name("body")
    {
        truth = combine(truth, direct_inner_truth(body, bytes)?);
    }
    Ok(truth)
}

fn direct_inner_truth(node: Node<'_>, bytes: &[u8]) -> Result<CfgTruth, RustSourceError> {
    let mut truth = CfgTruth::True;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "inner_attribute_item" {
            truth = combine(truth, attribute_truth(child, bytes)?);
        }
    }
    Ok(truth)
}

fn attribute_truth(attribute: Node<'_>, bytes: &[u8]) -> Result<CfgTruth, RustSourceError> {
    let text =
        std::str::from_utf8(&bytes[attribute.byte_range()]).map_err(RustSourceError::Utf8)?;
    let Some(meta) = strip_attribute(text) else {
        return Err(RustSourceError::Attribute {
            offset: attribute.start_byte(),
        });
    };
    meta_truth(meta, attribute.start_byte())
}

fn strip_attribute(attribute: &str) -> Option<&str> {
    let trimmed = attribute.trim();
    let body = trimmed
        .strip_prefix("#![")
        .or_else(|| trimmed.strip_prefix("#["))?;
    body.strip_suffix(']').map(str::trim)
}

pub(super) const fn combine(left: CfgTruth, right: CfgTruth) -> CfgTruth {
    match (left, right) {
        (CfgTruth::False, _) | (_, CfgTruth::False) => CfgTruth::False,
        (CfgTruth::Possible, _) | (_, CfgTruth::Possible) => CfgTruth::Possible,
        (CfgTruth::True, CfgTruth::True) => CfgTruth::True,
    }
}

fn merge_ranges(ranges: Vec<SourceRange>) -> Vec<SourceRange> {
    let mut merged: Vec<SourceRange> = Vec::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
            continue;
        }
        merged.push(range);
    }
    merged
}
