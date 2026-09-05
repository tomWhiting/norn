//! Fixed Markdown output and original-byte provenance regressions, including hostile controls.

use std::num::NonZeroUsize;

use super::*;
use crate::render::retained_text::TextLayout;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn markdown(original: &str) -> Result<RenderedMarkdown, MarkdownError> {
    static HIGHLIGHTER: std::sync::OnceLock<SyntaxHighlighter> = std::sync::OnceLock::new();
    super::render_markdown(original, HIGHLIGHTER.get_or_init(SyntaxHighlighter::new))
}

fn at(render: &RenderedMarkdown, needle: &str) -> Result<usize, std::io::Error> {
    render
        .styled
        .text()
        .find(needle)
        .ok_or_else(|| std::io::Error::other("expected displayed fixture text absent"))
}

fn style_at(render: &RenderedMarkdown, offset: usize) -> Result<TextStyle, std::io::Error> {
    render
        .styled
        .spans()
        .iter()
        .find(|span| span.range.contains(&offset))
        .map(|span| span.style)
        .ok_or_else(|| std::io::Error::other("expected display style absent"))
}

fn assert_mapping(render: &RenderedMarkdown, original: &str) {
    let mut end = 0;
    for span in &render.spans {
        assert_eq!(span.display.start, end);
        assert!(span.display.start < span.display.end);
        assert!(render.styled.text().is_char_boundary(span.display.start));
        assert!(render.styled.text().is_char_boundary(span.display.end));
        match &span.source {
            SourceMapping::Exact { original: source } => assert_eq!(
                original.get(source.clone()),
                render.styled.text().get(span.display.clone())
            ),
            SourceMapping::Transformed { original: source } => {
                assert!(source.start < source.end);
                assert!(original.get(source.clone()).is_some());
            }
            SourceMapping::Generated => {}
        }
        end = span.display.end;
    }
    assert_eq!(end, render.styled.text().len());
}

#[test]
fn nested_inline_styles_restore_their_parent_without_ansi() -> TestResult {
    let original = "*italic **bold** tail* ~~gone~~ `a&b`";
    let rendered = markdown(original)?;
    assert_eq!(rendered.styled.text(), "italic bold tail gone a&b");
    let bold = style_at(&rendered, at(&rendered, "bold")?)?;
    assert!(bold.attributes.contains(TextAttribute::Bold));
    assert!(bold.attributes.contains(TextAttribute::Italic));
    let tail = style_at(&rendered, at(&rendered, "tail")?)?;
    assert!(tail.attributes.contains(TextAttribute::Italic));
    assert!(!tail.attributes.contains(TextAttribute::Bold));
    assert!(
        style_at(&rendered, at(&rendered, "gone")?)?
            .attributes
            .contains(TextAttribute::Strike)
    );
    assert_eq!(
        style_at(&rendered, at(&rendered, "a&b")?)?.foreground,
        Some(CODE_FOREGROUND)
    );
    assert_mapping(&rendered, original);
    Ok(())
}

#[test]
fn block_quotes_nested_lists_headings_and_rules_are_compact() -> TestResult {
    let original = "# Title\n\n> quote\n> - one\n>   - two\n\n---";
    let rendered = markdown(original)?;
    assert_eq!(
        rendered.styled.text(),
        "Title\n│ quote\n│ • one\n│   • two\n───"
    );
    assert!(
        style_at(&rendered, 0)?
            .attributes
            .contains(TextAttribute::Bold)
    );
    assert!(!rendered.styled.text().contains("\n\n"));
    let bullet = at(&rendered, "•")?;
    assert_eq!(
        rendered.source_boundary(bullet, BoundaryAffinity::After)?,
        SourceBoundary::Generated
    );
    assert_mapping(&rendered, original);
    Ok(())
}

#[test]
fn task_and_ordered_markers_preserve_declared_structure() -> TestResult {
    let original = "- [x] done\n- [ ] later\n\n3. first\n4. second";
    let rendered = markdown(original)?;
    assert_eq!(
        rendered.styled.text(),
        "☑ done\n☐ later\n3. first\n4. second"
    );
    assert!(
        matches!(rendered.source_boundary(0, BoundaryAffinity::After)?, SourceBoundary::Transformed { original, .. } if original == (2..5))
    );
    assert_mapping(&rendered, original);
    Ok(())
}

#[test]
fn tables_keep_cell_order_and_header_styles_without_an_ansi_table_renderer() -> TestResult {
    let original = "| A | B |\n| :--- | ---: |\n| x | yy |";
    let rendered = markdown(original)?;
    assert_eq!(rendered.styled.text(), "A │ B\nx │ yy");
    assert!(
        style_at(&rendered, 0)?
            .attributes
            .contains(TextAttribute::Bold)
    );
    assert!(
        !style_at(&rendered, at(&rendered, "yy")?)?
            .attributes
            .contains(TextAttribute::Bold)
    );
    assert_mapping(&rendered, original);
    Ok(())
}

#[test]
fn code_math_links_images_and_html_are_visible_data() -> TestResult {
    for (original, expected) in [
        ("```rs\nlet x = 1;\n```", "let x = 1;\n"),
        ("    indented\n", "indented\n"),
        ("$x+y$ and $$z^2$$", "x+y and \nz^2"),
        (
            "[label](https://e.test/?x=1&y=2) ![alt](img.png)",
            "label (https://e.test/?x=1&y=2) [image: alt]",
        ),
        (
            "<b>x</b>\n\n<script>evil</script>",
            "<b>x</b>\n<script>evil</script>",
        ),
    ] {
        let rendered = markdown(original)?;
        assert_eq!(rendered.styled.text(), expected, "input {original:?}");
        assert_mapping(&rendered, original);
        assert!(!rendered.styled.text().contains('\u{1b}'));
    }
    let original = "[label](javascript:do_something)";
    let rendered = markdown(original)?;
    assert_eq!(rendered.styled.text(), "label (javascript:do_something)");
    assert!(matches!(
        rendered.source_boundary(at(&rendered, "javascript")?, BoundaryAffinity::After)?,
        SourceBoundary::Transformed { .. }
    ));
    Ok(())
}

#[test]
fn entities_escapes_smart_punctuation_and_code_normalization_never_claim_exact_offsets()
-> TestResult {
    let original = "a &amp; b \\* c -- d ` a\nb `";
    let rendered = markdown(original)?;
    assert_eq!(rendered.styled.text(), "a & b * c – d a b");
    for displayed in ["&", "*", "–", "a b"] {
        assert!(
            matches!(
                rendered.source_boundary(at(&rendered, displayed)?, BoundaryAffinity::After)?,
                SourceBoundary::Transformed { .. }
            ),
            "false exact mapping for {displayed}"
        );
    }
    let exact = at(&rendered, " c ")?;
    assert!(
        matches!(rendered.source_boundary(exact, BoundaryAffinity::After)?, SourceBoundary::Exact { original_offset } if original.get(original_offset..original_offset + 3) == Some(" c "))
    );
    assert_mapping(&rendered, original);
    Ok(())
}

#[test]
fn plain_and_markdown_controls_are_escaped_before_offsets_are_recorded() -> TestResult {
    let original = "a\u{1b}[31m\u{202e}é";
    for rendered in [render_plain(original)?, markdown(original)?] {
        assert_eq!(rendered.styled.text(), "a\\u{1b}[31m\\u{202e}é");
        assert!(
            matches!(rendered.source_boundary(2, BoundaryAffinity::After)?, SourceBoundary::Transformed { original, .. } if original == (1..2))
        );
        assert!(
            matches!(rendered.source_boundary(at(&rendered, "é")?, BoundaryAffinity::After)?, SourceBoundary::Exact { original_offset } if original_offset == original.len() - 2)
        );
        assert_mapping(&rendered, original);
    }
    let plain = render_plain("a\r\nb\t\n")?;
    assert_eq!(plain.styled.text(), "a\\u{d}\nb\t\n");
    assert_mapping(&plain, "a\r\nb\t\n");
    for rendered in [render_plain("a\0b")?, markdown("a\0b")?] {
        assert_eq!(rendered.styled.text(), "a\\u{0}b");
        assert_mapping(&rendered, "a\0b");
    }
    Ok(())
}

#[test]
fn grapheme_edges_and_boundary_affinity_preserve_original_identity() -> TestResult {
    let original = "e\u{301} 👩‍💻 **é**";
    let rendered = markdown(original)?;
    assert_eq!(rendered.styled.text(), "e\u{301} 👩‍💻 é");
    assert!(matches!(
        rendered.source_boundary(1, BoundaryAffinity::After),
        Err(MarkdownError::Boundary { offset: 1 })
    ));
    let emoji = at(&rendered, "👩")?;
    assert!(
        rendered
            .source_boundary(emoji + '👩'.len_utf8(), BoundaryAffinity::After)
            .is_err()
    );
    let end = rendered.styled.text().len();
    assert_eq!(
        rendered.source_boundary(end, BoundaryAffinity::Before)?,
        SourceBoundary::Exact {
            original_offset: original.len() - 2
        }
    );
    assert_eq!(
        rendered.source_boundary(end, BoundaryAffinity::After)?,
        SourceBoundary::Generated
    );
    assert!(
        rendered
            .source_boundary(end + 1, BoundaryAffinity::After)
            .is_err()
    );
    let width = NonZeroUsize::new(4).ok_or_else(|| std::io::Error::other("tab fixture is zero"))?;
    assert!(matches!(
        rendered.styled.layout(0, width)?,
        TextLayout::NoPaint
    ));
    let TextLayout::Rows(rows) = rendered.styled.layout(2, width)? else {
        return Err("rows absent".into());
    };
    for row in rows {
        for atom in row.atoms() {
            rendered.source_boundary(atom.bytes.start, BoundaryAffinity::After)?;
            rendered.source_boundary(atom.bytes.end, BoundaryAffinity::Before)?;
        }
    }
    assert_mapping(&rendered, original);
    Ok(())
}

#[test]
fn incomplete_streaming_syntax_remains_current_text_without_fabricated_completion() -> TestResult {
    for (original, expected) in [
        ("", ""),
        ("*unfinished", "*unfinished"),
        ("`unfinished", "`unfinished"),
        ("[label](incomplete", "[label](incomplete"),
        ("```rs\nlet x", "let x"),
        ("one\n\ntwo", "one\ntwo"),
    ] {
        let rendered = markdown(original)?;
        assert_eq!(rendered.styled.text(), expected);
        assert_mapping(&rendered, original);
    }
    assert_eq!(
        render_plain("")?.source_boundary(0, BoundaryAffinity::After)?,
        SourceBoundary::Generated
    );
    Ok(())
}

#[test]
fn fenced_code_reuses_direct_syntax_styles_and_preserves_source_bytes() -> TestResult {
    let highlighter = SyntaxHighlighter::new();
    let code = "fn main() {\n    let label = \"é🙂\";\n}\n";
    let original = format!("```rust\n{code}```");
    let rendered = super::render_markdown(&original, &highlighter)?;
    assert_eq!(rendered.styled.text(), code);
    assert_mapping(&rendered, &original);
    let direct = highlighter.highlight_spans(code, Some("rust"))?;
    for span in &direct {
        assert_eq!(style_at(&rendered, span.range.start)?, span.style);
    }
    assert!(direct.iter().any(|span| span.style != direct[0].style));
    assert!(matches!(
        rendered.source_boundary(0, BoundaryAffinity::After)?,
        SourceBoundary::Exact { original_offset: 8 }
    ));
    assert!(
        super::render_markdown(&original, &highlighter)?
            .styled
            .spans()
            == rendered.styled.spans()
    );
    Ok(())
}

#[test]
fn quoted_multiline_code_keeps_grammar_state_across_parser_fragments_and_escapes_controls()
-> TestResult {
    let highlighter = SyntaxHighlighter::new();
    let code = "/* first\n second */\nlet x = \"a\u{1b}b\";\n";
    let original = "> ```rust\n> /* first\n>  second */\n> let x = \"a\u{1b}b\";\n> ```";
    let rendered = super::render_markdown(original, &highlighter)?;
    assert_eq!(
        rendered.styled.text(),
        "│ /* first\n│  second */\n│ let x = \"a\\u{1b}b\";\n"
    );
    assert_mapping(&rendered, original);
    let direct = highlighter.highlight_spans(code, Some("rust"))?;
    let comment_byte = code
        .find("second")
        .ok_or_else(|| std::io::Error::other("comment fixture absent"))?;
    let expected = direct
        .iter()
        .find(|span| span.range.contains(&comment_byte))
        .ok_or_else(|| std::io::Error::other("comment syntax span absent"))?
        .style;
    assert_eq!(style_at(&rendered, at(&rendered, "second")?)?, expected);
    assert!(matches!(
        rendered.source_boundary(at(&rendered, "\\u{1b}")? + 2, BoundaryAffinity::After)?,
        SourceBoundary::Transformed { .. }
    ));
    Ok(())
}
