//! Production-only Rust LOC and content projection.

use serde::Serialize;
use thiserror::Error;
use tokei::{Config, LanguageType};
use tree_sitter::Node;

use crate::digest::{Digest, digest_bytes};
use crate::path::RepositoryPath;

use super::syntax::{RustSource, RustSourceError, SourceRange};

/// Deterministic production metrics for one Rust source file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ProductionMetrics {
    /// Tokei Rust code lines after test-only range masking.
    pub loc: u64,
    /// Path-bound production token projection.
    pub projection: Digest,
    /// Ranges proved absent from every production configuration.
    pub excluded: Vec<SourceRange>,
}

/// Analyze production LOC and token identity from owned bytes.
///
/// # Errors
///
/// Returns a parse/cfg failure, a numeric overflow, or an invariant failure if
/// a syntax token only partially overlaps an excluded item range.
pub fn production_metrics(
    path: &RepositoryPath,
    bytes: &[u8],
) -> Result<ProductionMetrics, LocError> {
    let source = RustSource::parse(bytes.to_vec())?;
    let excluded = source.test_only_ranges()?;
    let loc = production_loc(source.bytes(), &excluded)?;
    let projection = production_projection(path, &source, &excluded)?;
    Ok(ProductionMetrics {
        loc,
        projection,
        excluded,
    })
}

/// Production metric failures.
#[derive(Debug, Error)]
pub enum LocError {
    /// Rust parsing or cfg analysis failed closed.
    #[error("Rust source analysis failed")]
    Source(#[from] RustSourceError),
    /// Host line count cannot be represented in the stable format.
    #[error("Rust production LOC exceeds the stable integer range")]
    CountOverflow(#[source] std::num::TryFromIntError),
    /// A token partially crossed a supposedly item-aligned exclusion.
    #[error("Rust token partially overlaps a production exclusion")]
    PartialExclusion,
}

fn production_loc(bytes: &[u8], excluded: &[SourceRange]) -> Result<u64, LocError> {
    let mut masked = bytes.to_vec();
    for range in excluded {
        for byte in &mut masked[range.start()..range.end()] {
            if !matches!(*byte, b'\n' | b'\r') {
                *byte = b' ';
            }
        }
    }
    let statistics = LanguageType::Rust
        .parse_from_slice(masked, &Config::default())
        .summarise();
    u64::try_from(statistics.code).map_err(LocError::CountOverflow)
}

fn production_projection(
    path: &RepositoryPath,
    source: &RustSource,
    excluded: &[SourceRange],
) -> Result<Digest, LocError> {
    let mut encoded = Vec::new();
    append_field(&mut encoded, b"norn-production-projection-1")?;
    append_field(&mut encoded, path.as_str().as_bytes())?;
    append_tokens(source.root_node(), source.bytes(), excluded, &mut encoded)?;
    Ok(digest_bytes(&encoded))
}

fn append_tokens(
    node: Node<'_>,
    bytes: &[u8],
    excluded: &[SourceRange],
    encoded: &mut Vec<u8>,
) -> Result<(), LocError> {
    if excluded
        .iter()
        .any(|range| range.contains(node.start_byte(), node.end_byte()))
    {
        return Ok(());
    }
    if excluded
        .iter()
        .any(|range| range.overlaps(node.start_byte(), node.end_byte()))
        && node.child_count() == 0
    {
        return Err(LocError::PartialExclusion);
    }
    if node.kind() == "source_file" && node.child_count() == 0 {
        return Ok(());
    }
    if node.child_count() == 0 {
        append_field(encoded, node.kind().as_bytes())?;
        append_normalized_field(encoded, &bytes[node.byte_range()])?;
        return Ok(());
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        append_tokens(child, bytes, excluded, encoded)?;
    }
    Ok(())
}

fn append_field(output: &mut Vec<u8>, value: &[u8]) -> Result<(), LocError> {
    let length = u64::try_from(value.len()).map_err(LocError::CountOverflow)?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value);
    Ok(())
}

fn append_normalized_field(output: &mut Vec<u8>, value: &[u8]) -> Result<(), LocError> {
    let mut normalized = Vec::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < value.len() {
        if value[cursor..].starts_with(b"\r\n") {
            normalized.push(b'\n');
            cursor += 2;
        } else {
            normalized.push(value[cursor]);
            cursor += 1;
        }
    }
    append_field(output, &normalized)
}
