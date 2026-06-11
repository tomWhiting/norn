//! Norn-local entity-extraction abstraction for patch tier-1 resolution.
//!
//! [`ApplyPatchTool`](super::patch::ApplyPatchTool) resolves unified-diff
//! hunks by first locating the entity named in a hunk's `@@` semantic anchor
//! and scoping the context search to that entity's line range. This module
//! defines the abstraction that supplies those entities:
//!
//! * [`ExtractedEntity`] — a lightweight, Norn-owned structural entity. It is
//!   deliberately *not* a re-export of libyggd's `StructuralEntity`, so the
//!   patch resolver stays decoupled from libyggd's API and feature graph.
//! * [`EntityExtractor`] — the trait the resolver depends on. A `None` return
//!   means "this source/language is not supported", which makes the resolver
//!   skip tier 1 and fall through to its context-anchored tiers.
//!
//! [`LibygdEntityExtractor`], gated behind the `libyggd-ast` feature, is the
//! production implementation; it bridges to `libyggd::ast::extract_entities`.
//! Feature-disabled builds and unit tests use mock extractors, or no
//! extractor at all.

use std::ops::Range;
use std::path::Path;

/// A lightweight structural entity used for patch tier-1 anchor resolution.
///
/// Line ranges follow libyggd's convention: 1-indexed, with both `start` and
/// `end` inclusive — the entity occupies lines `start..=end`.
#[derive(Clone, Debug)]
pub struct ExtractedEntity {
    /// The entity's name (e.g. a function name), or `None` for anonymous
    /// entities.
    pub name: Option<String>,
    /// Fully qualified name including parent scope (e.g. `MyStruct::method`).
    pub qualified_name: String,
    /// Human-readable entity kind (e.g. `fn`, `struct`, `class`).
    pub kind: String,
    /// Byte range of the entity within the source.
    pub byte_range: Range<usize>,
    /// 1-indexed, inclusive line range (`start..=end`).
    pub line_range: Range<usize>,
}

/// Locates structural entities in source for patch tier-1 resolution.
///
/// Implementations return `None` when the source's language is not supported,
/// signalling the resolver to skip tier 1 and start at the context-anchored
/// tier 2.
pub trait EntityExtractor: Send + Sync {
    /// Extract structural entities from `source`, using `path` to detect the
    /// language. Returns `None` for unsupported languages, `Some` (possibly
    /// empty) otherwise.
    fn extract(&self, source: &str, path: &Path) -> Option<Vec<ExtractedEntity>>;
}

/// [`EntityExtractor`] backed by libyggd's tree-sitter entity extraction.
///
/// Detects the language from the file path via
/// [`libyggd::ast::detect_language`], extracts entities with
/// [`libyggd::ast::extract_entities`], flattens nested entities (so methods
/// inside `impl`/`class` containers are matchable as tier-1 anchors), and maps
/// each `StructuralEntity` into an [`ExtractedEntity`].
#[cfg(feature = "libyggd-ast")]
#[derive(Clone, Copy, Debug, Default)]
pub struct LibygdEntityExtractor;

#[cfg(feature = "libyggd-ast")]
impl EntityExtractor for LibygdEntityExtractor {
    fn extract(&self, source: &str, path: &Path) -> Option<Vec<ExtractedEntity>> {
        let language = libyggd::ast::detect_language(path)?;
        let entities = match libyggd::ast::extract_entities(source, language) {
            Ok(entities) => entities,
            Err(e) => {
                // An extraction *error* is not the same as "language not
                // supported": log it before degrading to the contract's
                // `None` (the resolver falls through to context tiers).
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "entity extraction failed; skipping tier-1 anchor resolution for this file",
                );
                return None;
            }
        };
        Some(
            libyggd::ast::flatten_entities(&entities)
                .into_iter()
                .map(convert_entity)
                .collect(),
        )
    }
}

/// Map a libyggd [`StructuralEntity`](libyggd::ast::StructuralEntity) into a
/// Norn [`ExtractedEntity`], rendering the entity kind via its `Display` impl.
#[cfg(feature = "libyggd-ast")]
fn convert_entity(entity: &libyggd::ast::StructuralEntity) -> ExtractedEntity {
    ExtractedEntity {
        name: entity.name.clone(),
        qualified_name: entity.qualified_name.clone(),
        kind: entity.kind.to_string(),
        byte_range: entity.byte_range.clone(),
        line_range: entity.line_range.clone(),
    }
}

#[cfg(all(test, feature = "libyggd-ast"))]
mod libyggd_tests {
    use super::*;

    #[test]
    fn extracts_rust_entities_with_fields_converted() -> Result<(), String> {
        let src = "\
fn alpha() {
    let x = 1;
}

struct Beta {
    field: u8,
}
";
        let entities = LibygdEntityExtractor
            .extract(src, Path::new("sample.rs"))
            .ok_or("rust is a supported language")?;

        let alpha = entities
            .iter()
            .find(|e| e.name.as_deref() == Some("alpha"))
            .ok_or("alpha function extracted")?;
        assert_eq!(alpha.kind, "fn");
        assert_eq!(alpha.qualified_name, "alpha");
        assert!(alpha.line_range.start >= 1, "1-indexed line range");
        assert!(
            alpha.byte_range.start < alpha.byte_range.end,
            "non-empty byte range",
        );

        assert!(
            entities
                .iter()
                .any(|e| e.name.as_deref() == Some("Beta") && e.kind == "struct"),
            "struct Beta extracted: {entities:?}",
        );
        Ok(())
    }

    #[test]
    fn flattens_nested_methods() -> Result<(), String> {
        let src = "\
struct Foo;

impl Foo {
    fn bar(&self) -> u8 {
        1
    }
}
";
        let entities = LibygdEntityExtractor
            .extract(src, Path::new("nested.rs"))
            .ok_or("rust is supported")?;
        assert!(
            entities.iter().any(|e| e.name.as_deref() == Some("bar")),
            "flattening must surface the nested method: {entities:?}",
        );
        Ok(())
    }

    #[test]
    fn extracts_python_entities() -> Result<(), String> {
        let src = "\
def greet():
    return 1

class Widget:
    def method(self):
        return 2
";
        let entities = LibygdEntityExtractor
            .extract(src, Path::new("sample.py"))
            .ok_or("python is a supported language")?;
        assert!(
            entities.iter().any(|e| e.name.as_deref() == Some("greet")),
            "top-level function extracted: {entities:?}",
        );
        assert!(
            entities.iter().any(|e| e.name.as_deref() == Some("Widget")),
            "class extracted: {entities:?}",
        );
        assert!(
            entities.iter().any(|e| e.name.as_deref() == Some("method")),
            "flattening must surface the nested method: {entities:?}",
        );
        Ok(())
    }

    #[test]
    fn extracts_typescript_entities() -> Result<(), String> {
        let src = "\
function compute(): number {
  return 1;
}

class Service {
  run(): void {}
}
";
        let entities = LibygdEntityExtractor
            .extract(src, Path::new("sample.ts"))
            .ok_or("typescript is a supported language")?;
        assert!(
            entities
                .iter()
                .any(|e| e.name.as_deref() == Some("compute")),
            "top-level function extracted: {entities:?}",
        );
        assert!(
            entities
                .iter()
                .any(|e| e.name.as_deref() == Some("Service")),
            "class extracted: {entities:?}",
        );
        Ok(())
    }

    #[test]
    fn unsupported_extension_returns_none() {
        assert!(
            LibygdEntityExtractor
                .extract("plain text", Path::new("notes.txt"))
                .is_none(),
            "unknown extension yields None",
        );
        assert!(
            LibygdEntityExtractor
                .extract("data", Path::new("noext"))
                .is_none(),
            "extensionless path yields None",
        );
    }
}
