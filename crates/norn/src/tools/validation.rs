//! Shared validation helpers for file-modification tools.
//!
//! Provides [`count_code_lines`] — a tokei-backed code-line count that
//! excludes comments and blank lines. Falls back to non-blank line
//! counting when tokei does not recognise the file's language.

use std::path::Path;

/// Counts code lines for `content`, excluding comments and blank lines.
///
/// Uses [`tokei::LanguageType::from_path`] for language detection and
/// [`tokei::LanguageType::parse_from_str`] for in-memory counting (no
/// disk I/O). When the path's extension is not recognised by tokei,
/// falls back to counting lines whose trimmed form is non-empty.
///
/// Always returns a value — the fallback path never fails.
#[must_use]
pub fn count_code_lines(path: &Path, content: &str) -> u64 {
    let config = tokei::Config::default();
    if let Some(language) = tokei::LanguageType::from_path(path, &config) {
        let stats = language.parse_from_str(content, &config);
        let code = u64::try_from(stats.code).unwrap_or(u64::MAX);
        if code > 0 {
            return code;
        }
    }
    let non_blank = content.lines().filter(|l| !l.trim().is_empty()).count();
    u64::try_from(non_blank).unwrap_or(u64::MAX)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::missing_const_for_fn,
    clippy::uninlined_format_args
)]
mod tests {
    use super::*;

    #[test]
    fn rust_file_counts_code_lines_excluding_comments_and_blanks() {
        let content = "// a comment\nfn main() {\n    let x = 1;\n}\n\n// trailing\n";
        let count = count_code_lines(Path::new("foo.rs"), content);
        // 3 code lines: fn main() {, let x = 1;, }
        assert_eq!(count, 3);
    }

    #[test]
    fn python_file_counts_code_lines_excluding_comments() {
        let content = "# header\n\ndef f():\n    return 1\n# tail\n";
        let count = count_code_lines(Path::new("foo.py"), content);
        assert_eq!(count, 2);
    }

    #[test]
    fn unrecognised_extension_falls_back_to_non_blank_line_count() {
        let content = "alpha\n\nbeta\n\n\ngamma\n";
        let count = count_code_lines(Path::new("foo.xyz123"), content);
        assert_eq!(count, 3);
    }

    #[test]
    fn empty_content_returns_zero() {
        assert_eq!(count_code_lines(Path::new("foo.rs"), ""), 0);
        assert_eq!(count_code_lines(Path::new("foo.unknown"), ""), 0);
    }

    #[test]
    fn rust_file_with_100_code_50_comments_20_blanks_returns_100() {
        use std::fmt::Write as _;

        let mut content = String::new();
        for i in 0..100 {
            let _ = writeln!(content, "let x_{i} = {i};");
        }
        for _ in 0..50 {
            content.push_str("// comment line\n");
        }
        for _ in 0..20 {
            content.push('\n');
        }
        let count = count_code_lines(Path::new("foo.rs"), &content);
        assert_eq!(count, 100);
    }

    #[test]
    fn markdown_falls_back_to_non_blank_when_tokei_reports_zero_code() {
        let content = "# Title\n\nSome paragraph text.\n\n- item 1\n- item 2\n";
        let count = count_code_lines(Path::new("README.md"), content);
        assert!(count > 0, "markdown line_count should not be 0");
        assert_eq!(count, 4);
    }
}
