//! Stable Rust item identity for production-to-test reclassification checks.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::Serialize;
use thiserror::Error;
use tree_sitter::Node;

use crate::digest::{Digest, digest_bytes};
use crate::finding::{ByteSpan, ByteSpanError};
use crate::path::RepositoryPath;

use super::identifier;
use super::syntax::{RustSource, RustSourceError, SourceRange};

/// One stable Rust item group retained for origin comparison.
///
/// Groups aggregate every occurrence with the same path-bound structural
/// identity and normalized content. Equality and ordering intentionally omit
/// diagnostic spans: moving comments or swapping identical production and
/// test-only occurrences must not change the stable comparison surface.
#[derive(Clone, Debug, Serialize)]
pub struct RustItemProjection {
    /// Path-bound structural identity without a source-order ordinal.
    base_identity: Digest,
    /// Normalized syntax/token content without attached outer attributes.
    content: Digest,
    /// Number of occurrences reachable in a production configuration.
    production_count: u32,
    /// Number of occurrences proved reachable only in tests.
    test_only_count: u32,
    /// Sorted locations for production occurrences, retained as evidence.
    production_spans: Vec<ByteSpan>,
    /// Sorted locations for test-only occurrences, retained as evidence.
    test_only_spans: Vec<ByteSpan>,
}

impl RustItemProjection {
    /// Return the path-bound structural identity.
    #[must_use]
    pub const fn base_identity(&self) -> Digest {
        self.base_identity
    }

    /// Return the normalized syntax/token content digest.
    #[must_use]
    pub const fn content(&self) -> Digest {
        self.content
    }

    /// Return the production occurrence multiplicity.
    #[must_use]
    pub const fn production_count(&self) -> u32 {
        self.production_count
    }

    /// Return the test-only occurrence multiplicity.
    #[must_use]
    pub const fn test_only_count(&self) -> u32 {
        self.test_only_count
    }

    /// Borrow the sorted production occurrence locations.
    #[must_use]
    pub fn production_spans(&self) -> &[ByteSpan] {
        &self.production_spans
    }

    /// Borrow the sorted test-only occurrence locations.
    #[must_use]
    pub fn test_only_spans(&self) -> &[ByteSpan] {
        &self.test_only_spans
    }

    /// Reclassify every occurrence when Cargo proves the whole root test-only.
    ///
    /// # Errors
    ///
    /// Returns a closed overflow failure if the aggregate multiplicity cannot
    /// be represented by the stable count format.
    pub(crate) fn force_test_only(&mut self) -> Result<(), RustItemProjectionError> {
        self.test_only_count = self
            .test_only_count
            .checked_add(self.production_count)
            .ok_or(RustItemProjectionError::CountOverflow)?;
        self.production_count = 0;
        self.test_only_spans.append(&mut self.production_spans);
        self.test_only_spans.sort_unstable();
        Ok(())
    }
}

impl PartialEq for RustItemProjection {
    fn eq(&self, other: &Self) -> bool {
        self.stable_fields() == other.stable_fields()
    }
}

impl Eq for RustItemProjection {}

impl PartialOrd for RustItemProjection {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RustItemProjection {
    fn cmp(&self, other: &Self) -> Ordering {
        self.stable_fields().cmp(&other.stable_fields())
    }
}

impl RustItemProjection {
    const fn stable_fields(&self) -> (Digest, Digest, u32, u32) {
        (
            self.base_identity,
            self.content,
            self.production_count,
            self.test_only_count,
        )
    }
}

/// Analyze every item that can be compared across production/test predicates.
///
/// Attached outer attributes are sibling syntax and therefore do not change an
/// item's content digest. Adding `cfg(test)` changes its classification while
/// retaining identity and content, which makes reclassification observable.
///
/// # Errors
///
/// Returns a closed parse/cfg, span, or aggregate-count overflow failure.
pub fn rust_item_projections(
    path: &RepositoryPath,
    bytes: &[u8],
) -> Result<Vec<RustItemProjection>, RustItemProjectionError> {
    let source = RustSource::parse(bytes.to_vec())?;
    let excluded = source.test_only_ranges()?;
    let mut drafts = Vec::new();
    collect_items(
        source.root_node(),
        source.bytes(),
        path,
        &excluded,
        &mut drafts,
    )?;
    let mut groups = BTreeMap::<(Digest, Digest), ItemGroupDraft>::new();
    for draft in drafts {
        let group = groups
            .entry((draft.base_identity, draft.content))
            .or_default();
        let span = byte_span(draft.start, draft.end)?;
        if draft.production {
            group.push_production(span)?;
        } else {
            group.push_test_only(span)?;
        }
    }
    Ok(groups
        .into_iter()
        .map(|((base_identity, content), group)| {
            RustItemProjection::from_group(base_identity, content, group)
        })
        .collect())
}

/// Rust item projection failures.
#[derive(Debug, Error)]
pub enum RustItemProjectionError {
    /// Parsing or production-range analysis failed.
    #[error("Rust item projection source analysis failed")]
    Source(#[from] RustSourceError),
    /// A source span cannot be represented by the stable format.
    #[error("Rust item projection span is invalid")]
    Span(#[from] ByteSpanError),
    /// A source offset or field length exceeds the stable format.
    #[error("Rust item projection value exceeds u64")]
    Overflow(#[source] std::num::TryFromIntError),
    /// A structural/content group has more occurrences than the stable count permits.
    #[error("Rust item projection group count exceeds u32")]
    CountOverflow,
}

struct ItemDraft {
    base_identity: Digest,
    content: Digest,
    start: usize,
    end: usize,
    production: bool,
}

#[derive(Default)]
struct ItemGroupDraft {
    production_count: u32,
    test_only_count: u32,
    production_spans: Vec<ByteSpan>,
    test_only_spans: Vec<ByteSpan>,
}

impl ItemGroupDraft {
    fn push_production(&mut self, span: ByteSpan) -> Result<(), RustItemProjectionError> {
        self.production_count = self
            .production_count
            .checked_add(1)
            .ok_or(RustItemProjectionError::CountOverflow)?;
        self.production_spans.push(span);
        Ok(())
    }

    fn push_test_only(&mut self, span: ByteSpan) -> Result<(), RustItemProjectionError> {
        self.test_only_count = self
            .test_only_count
            .checked_add(1)
            .ok_or(RustItemProjectionError::CountOverflow)?;
        self.test_only_spans.push(span);
        Ok(())
    }
}

impl RustItemProjection {
    fn from_group(base_identity: Digest, content: Digest, mut group: ItemGroupDraft) -> Self {
        group.production_spans.sort_unstable();
        group.test_only_spans.sort_unstable();
        Self {
            base_identity,
            content,
            production_count: group.production_count,
            test_only_count: group.test_only_count,
            production_spans: group.production_spans,
            test_only_spans: group.test_only_spans,
        }
    }
}

fn collect_items(
    node: Node<'_>,
    bytes: &[u8],
    path: &RepositoryPath,
    excluded: &[SourceRange],
    drafts: &mut Vec<ItemDraft>,
) -> Result<(), RustItemProjectionError> {
    if is_item(node.kind()) {
        drafts.push(ItemDraft {
            base_identity: item_identity(node, bytes, path)?,
            content: node_digest(node, bytes)?,
            start: node.start_byte(),
            end: node.end_byte(),
            production: !excluded
                .iter()
                .any(|range| range.contains(node.start_byte(), node.end_byte())),
        });
    }
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        collect_items(child, bytes, path, excluded, drafts)?;
    }
    Ok(())
}

fn item_identity(
    node: Node<'_>,
    bytes: &[u8],
    path: &RepositoryPath,
) -> Result<Digest, RustItemProjectionError> {
    let mut chain = Vec::new();
    let mut current = Some(node);
    while let Some(candidate) = current {
        if is_item(candidate.kind()) {
            chain.push(identity_component(candidate, bytes)?);
        }
        current = candidate.parent();
    }
    chain.reverse();
    let mut framed = Vec::new();
    append_field(&mut framed, b"norn-rust-item-1")?;
    append_field(&mut framed, path.as_str().as_bytes())?;
    for component in chain {
        append_field(&mut framed, component.as_bytes())?;
    }
    Ok(digest_bytes(&framed))
}

fn identity_component(node: Node<'_>, bytes: &[u8]) -> Result<Digest, RustItemProjectionError> {
    let mut framed = Vec::new();
    append_field(&mut framed, node.kind().as_bytes())?;
    if let Some(name) = node.child_by_field_name("name") {
        append_field(
            &mut framed,
            identifier::canonical_bytes(&bytes[name.byte_range()]),
        )?;
    } else if matches!(node.kind(), "impl_item" | "foreign_mod_item") {
        append_header_tokens(&mut framed, node, bytes)?;
    }
    Ok(digest_bytes(&framed))
}

fn append_header_tokens(
    output: &mut Vec<u8>,
    node: Node<'_>,
    bytes: &[u8],
) -> Result<(), RustItemProjectionError> {
    let body_start = node
        .child_by_field_name("body")
        .map(|body| body.start_byte());
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if body_start.is_some_and(|start| child.start_byte() >= start) {
            break;
        }
        append_node(output, child, bytes)?;
    }
    Ok(())
}

fn node_digest(node: Node<'_>, bytes: &[u8]) -> Result<Digest, RustItemProjectionError> {
    let mut framed = Vec::new();
    append_node(&mut framed, node, bytes)?;
    Ok(digest_bytes(&framed))
}

fn append_node(
    output: &mut Vec<u8>,
    node: Node<'_>,
    bytes: &[u8],
) -> Result<(), RustItemProjectionError> {
    if matches!(node.kind(), "line_comment" | "block_comment") {
        return Ok(());
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
    if children.is_empty() {
        append_field(output, node.kind().as_bytes())?;
        let raw = &bytes[node.byte_range()];
        let semantic = if node.kind().ends_with("identifier") {
            identifier::canonical_bytes(raw)
        } else {
            raw
        };
        append_field(output, &normalized_bytes(semantic))?;
        return Ok(());
    }
    for child in children {
        append_node(output, child, bytes)?;
    }
    Ok(())
}

fn append_field(output: &mut Vec<u8>, value: &[u8]) -> Result<(), RustItemProjectionError> {
    let length = u64::try_from(value.len()).map_err(RustItemProjectionError::Overflow)?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value);
    Ok(())
}

fn normalized_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor..].starts_with(b"\r\n") {
            normalized.push(b'\n');
            cursor += 2;
        } else {
            normalized.push(bytes[cursor]);
            cursor += 1;
        }
    }
    normalized
}

fn byte_span(start: usize, end: usize) -> Result<ByteSpan, RustItemProjectionError> {
    let start = u64::try_from(start).map_err(RustItemProjectionError::Overflow)?;
    let end = u64::try_from(end).map_err(RustItemProjectionError::Overflow)?;
    ByteSpan::new(start, end).map_err(RustItemProjectionError::Span)
}

fn is_item(kind: &str) -> bool {
    (kind.ends_with("_item") && !matches!(kind, "attribute_item" | "inner_attribute_item"))
        || matches!(
            kind,
            "macro_definition"
                | "macro_invocation"
                | "use_declaration"
                | "extern_crate_declaration"
        )
}
