//! Shared tree-sitter helpers for the file tools.
//!
//! This module is private to the `tools` module and provides language
//! detection from file extensions, tree-sitter parsing, error walking,
//! and tree-walking helpers used by `Write`, `Edit`, `Search`, and other
//! tools that need to validate syntax or report enclosing symbols. Patch
//! tier-1 entity resolution lives elsewhere (`patch_entity`), so this module
//! deliberately carries no entity-extraction stack.

use std::path::Path;

use tree_sitter::{Language, Node, Parser, Tree, TreeCursor};

/// Languages for which `tools::ast` performs syntax validation and symbol
/// reporting.
///
/// The set is deliberately narrow for the file tools' built-in tree-sitter
/// support — it stays available with no optional features enabled. Richer,
/// many-language entity extraction is supplied separately via the
/// [`EntityExtractor`](super::patch_entity::EntityExtractor) abstraction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SyntaxLanguage {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Json,
}

impl SyntaxLanguage {
    /// Returns the tree-sitter `Language` for this enum variant.
    pub(super) fn tree_sitter_language(self) -> Language {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Json => tree_sitter_json::LANGUAGE.into(),
        }
    }

    /// Maps a path's extension (case-insensitive) to a supported language.
    pub(super) fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "rs" => Some(Self::Rust),
            "py" => Some(Self::Python),
            "js" => Some(Self::JavaScript),
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// A syntax error reported by tree-sitter, with 1-based line and column.
#[derive(Clone, Debug)]
pub(super) struct SyntaxError {
    /// 1-based line number of the error or missing-token position.
    pub line: usize,
    /// 1-based column of the error or missing-token position.
    pub column: usize,
    /// Tree-sitter node kind that holds the problem.
    pub kind: String,
    /// True for `MISSING` nodes; false for `ERROR` nodes.
    pub missing: bool,
}

impl SyntaxError {
    /// Renders a human-readable description such as
    /// `line 12, column 5: missing }` or `line 4, column 1: ERROR ('expression')`.
    pub(super) fn render(&self) -> String {
        if self.missing {
            format!(
                "line {}, column {}: missing {}",
                self.line, self.column, self.kind
            )
        } else {
            format!(
                "line {}, column {}: syntax error ({})",
                self.line, self.column, self.kind
            )
        }
    }
}

/// Outcome of an AST validation pass over a source string.
#[derive(Clone, Debug)]
pub(super) enum AstCheck {
    /// The file extension is not in the supported set; no validation performed.
    Unsupported,
    /// Validation ran and found no errors.
    Pass,
    /// Validation ran and found one or more errors or missing tokens.
    Fail {
        /// Concrete syntax errors discovered during the walk.
        errors: Vec<SyntaxError>,
    },
}

/// Parses `source` for the language matching `path`'s extension and
/// reports any `ERROR` or `MISSING` nodes.
///
/// Returns `Unsupported` for extensions outside the v1 set so callers
/// can treat unsupported files as "no validation" (Pass-equivalent)
/// rather than as a failure.
pub(super) fn check_syntax(path: &Path, source: &str) -> AstCheck {
    let Some(language) = SyntaxLanguage::from_path(path) else {
        return AstCheck::Unsupported;
    };

    let Some(tree) = parse(source, language) else {
        // Setting the language or parsing returned None — treat as a
        // single, top-of-file syntax error so the caller surfaces it
        // rather than silently passing.
        return AstCheck::Fail {
            errors: vec![SyntaxError {
                line: 1,
                column: 1,
                kind: "parser_failure".to_string(),
                missing: false,
            }],
        };
    };

    let mut errors = Vec::new();
    let mut cursor = tree.walk();
    collect_errors(&mut cursor, &mut errors);

    if errors.is_empty() {
        AstCheck::Pass
    } else {
        AstCheck::Fail { errors }
    }
}

/// Parses `source` for the given supported language, returning the tree
/// or `None` if the parser refused to set the language or returned no tree.
pub(super) fn parse(source: &str, language: SyntaxLanguage) -> Option<Tree> {
    let mut parser = Parser::new();
    parse_with(&mut parser, source, language)
}

/// Parses `source` with a caller-owned parser, so multi-file callers can
/// reuse one `Parser` allocation across an entire batch instead of
/// constructing a fresh parser per file.
pub(super) fn parse_with(
    parser: &mut Parser,
    source: &str,
    language: SyntaxLanguage,
) -> Option<Tree> {
    parser.set_language(&language.tree_sitter_language()).ok()?;
    parser.parse(source, None)
}

/// Walks the tree from the cursor's current position depth-first,
/// collecting `is_error()` and `is_missing()` nodes.
fn collect_errors(cursor: &mut TreeCursor<'_>, out: &mut Vec<SyntaxError>) {
    loop {
        let node = cursor.node();
        if node.is_error() || node.is_missing() {
            let point = node.start_position();
            out.push(SyntaxError {
                line: point.row + 1,
                column: point.column + 1,
                kind: node.kind().to_string(),
                missing: node.is_missing(),
            });
        }

        if cursor.goto_first_child() {
            continue;
        }

        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

/// Returns a label such as `fn target_function` or `impl Bar` for a
/// container node, by combining the node's kind and its `name` child.
fn label_for_container(node: Node<'_>, source: &str) -> String {
    let prefix = match node.kind() {
        "function_item" | "function_declaration" | "function_definition" => "fn",
        "impl_item" => "impl",
        "struct_item" => "struct",
        "enum_item" => "enum",
        "trait_item" => "trait",
        "mod_item" => "mod",
        "class_declaration" | "class_definition" => "class",
        "method_definition" => "method",
        "arrow_function" => "arrow_function",
        other => other,
    };

    let name_node = node.child_by_field_name("name");
    let name = name_node.and_then(|n| source.get(n.start_byte()..n.end_byte()));

    match name {
        Some(name) if !name.is_empty() => format!("{prefix} {name}"),
        _ => prefix.to_string(),
    }
}

/// Finds container symbols (functions, structs, classes, ...) that
/// enclose the byte range `[start, end)` of `source` for `path`'s language.
///
/// Returns the labels deepest-first, deduplicated, so the most specific
/// containing symbol is listed first. Empty when the language is
/// unsupported or no container nodes enclose the range.
pub(super) fn containing_symbols(
    path: &Path,
    source: &str,
    start_byte: usize,
    end_byte: usize,
) -> Vec<String> {
    let Some(language) = SyntaxLanguage::from_path(path) else {
        return Vec::new();
    };
    let Some(tree) = parse(source, language) else {
        return Vec::new();
    };

    // Container node kinds whose names identify an enclosing symbol.
    let kinds: &[&str] = match language {
        SyntaxLanguage::Rust => &[
            "function_item",
            "impl_item",
            "struct_item",
            "enum_item",
            "trait_item",
            "mod_item",
        ],
        SyntaxLanguage::Python => &["function_definition", "class_definition"],
        SyntaxLanguage::JavaScript | SyntaxLanguage::TypeScript => &[
            "function_declaration",
            "class_declaration",
            "method_definition",
            "arrow_function",
        ],
        SyntaxLanguage::Json => &[],
    };
    if kinds.is_empty() {
        return Vec::new();
    }

    let root = tree.root_node();
    let descendant = root.descendant_for_byte_range(start_byte, end_byte.max(start_byte));
    let mut current = descendant;
    let mut labels: Vec<String> = Vec::new();

    while let Some(node) = current {
        if kinds.contains(&node.kind()) {
            let label = label_for_container(node, source);
            if !labels.contains(&label) {
                labels.push(label);
            }
        }
        current = node.parent();
    }

    labels
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::clone_on_ref_ptr,
    clippy::no_effect_underscore_binding,
    clippy::useless_vec,
    clippy::missing_const_for_fn,
    clippy::duration_suboptimal_units,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::redundant_closure_for_method_calls,
    clippy::used_underscore_items,
    clippy::unnecessary_literal_bound,
    clippy::items_after_statements,
    clippy::err_expect,
    clippy::get_unwrap,
    clippy::doc_markdown,
    clippy::unnecessary_trailing_comma,
    clippy::uninlined_format_args,
    clippy::wildcard_enum_match_arm,
    clippy::collapsible_if,
    clippy::match_wildcard_for_single_variants
)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detects_languages_by_extension_case_insensitive() {
        assert_eq!(
            SyntaxLanguage::from_path(Path::new("foo.rs")),
            Some(SyntaxLanguage::Rust)
        );
        assert_eq!(
            SyntaxLanguage::from_path(Path::new("foo.RS")),
            Some(SyntaxLanguage::Rust)
        );
        assert_eq!(
            SyntaxLanguage::from_path(Path::new("foo.py")),
            Some(SyntaxLanguage::Python)
        );
        assert_eq!(
            SyntaxLanguage::from_path(Path::new("foo.js")),
            Some(SyntaxLanguage::JavaScript)
        );
        assert_eq!(
            SyntaxLanguage::from_path(Path::new("foo.ts")),
            Some(SyntaxLanguage::TypeScript)
        );
        assert_eq!(
            SyntaxLanguage::from_path(Path::new("foo.mts")),
            Some(SyntaxLanguage::TypeScript)
        );
        assert_eq!(
            SyntaxLanguage::from_path(Path::new("foo.cts")),
            Some(SyntaxLanguage::TypeScript)
        );
        assert_eq!(
            SyntaxLanguage::from_path(Path::new("foo.json")),
            Some(SyntaxLanguage::Json)
        );
        assert_eq!(SyntaxLanguage::from_path(Path::new("foo.txt")), None);
        assert_eq!(SyntaxLanguage::from_path(Path::new("noext")), None);
    }

    #[test]
    fn unsupported_extension_returns_unsupported() {
        match check_syntax(Path::new("hello.txt"), "anything goes") {
            AstCheck::Unsupported => {}
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn valid_rust_passes() {
        let src = "fn main() { let x = 1; }\n";
        match check_syntax(Path::new("a.rs"), src) {
            AstCheck::Pass => {}
            other => panic!("expected Pass, got {other:?}"),
        }
    }

    #[test]
    fn missing_brace_fails_with_line_number() {
        let src = "fn main() { let x = 1;\n";
        let result = check_syntax(Path::new("a.rs"), src);
        match result {
            AstCheck::Fail { errors } => {
                assert!(!errors.is_empty(), "expected at least one syntax error");
                let rendered = errors[0].render();
                assert!(
                    rendered.contains("line "),
                    "rendered error '{rendered}' missing line marker"
                );
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[test]
    fn invalid_json_fails() {
        let src = "{ \"a\": 1, \"b\": }";
        match check_syntax(Path::new("a.json"), src) {
            AstCheck::Fail { errors } => {
                assert!(!errors.is_empty());
            }
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    #[test]
    fn containing_symbols_finds_function_in_rust() {
        let src = "fn alpha() {\n    let inner = 1;\n}\n";
        let path = PathBuf::from("a.rs");
        // Locate `let inner` byte offset for the test.
        let start = src.find("let inner").unwrap();
        let end = start + "let inner".len();
        let labels = containing_symbols(&path, src, start, end);
        assert!(
            labels.iter().any(|l| l.contains("alpha")),
            "labels {labels:?} did not contain 'alpha'"
        );
    }

    #[test]
    fn containing_symbols_unsupported_is_empty() {
        let labels = containing_symbols(Path::new("a.txt"), "anything", 0, 5);
        assert!(labels.is_empty());
    }
}
