//! Assistant JSON field visibility, decoded Markdown and exact original-byte mapping regressions.

use super::*;
use crate::render::retained_markdown::{BoundaryAffinity, SourceBoundary};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn highlighter() -> &'static SyntaxHighlighter {
    static HIGHLIGHTER: std::sync::OnceLock<SyntaxHighlighter> = std::sync::OnceLock::new();
    HIGHLIGHTER.get_or_init(SyntaxHighlighter::new)
}

fn render(original: &str, secondary: bool) -> Result<RenderedMarkdown, StructuredError> {
    render_structured(original, secondary, highlighter())
}

fn at(text: &str, needle: &str) -> Result<usize, std::io::Error> {
    text.find(needle)
        .ok_or_else(|| std::io::Error::other("expected fixture text absent"))
}

fn exact_at(rendered: &RenderedMarkdown, original: &str, needle: &str) -> TestResult {
    let displayed = at(rendered.styled.text(), needle)?;
    assert_eq!(
        rendered.source_boundary(displayed, BoundaryAffinity::After)?,
        SourceBoundary::Exact {
            original_offset: at(original, needle)?
        }
    );
    Ok(())
}

fn transformed_at(
    rendered: &RenderedMarkdown,
    original: &str,
    displayed: &str,
    raw: &str,
) -> TestResult {
    let offset = at(rendered.styled.text(), displayed)?;
    let start = at(original, raw)?;
    let boundary = rendered.source_boundary(offset, BoundaryAffinity::After)?;
    assert!(
        matches!(boundary, SourceBoundary::Transformed { original, .. } if original == (start..start + raw.len()))
    );
    Ok(())
}

fn assert_mapping(rendered: &RenderedMarkdown, original: &str) {
    let mut end = 0;
    for span in &rendered.spans {
        assert_eq!(span.display.start, end);
        assert!(span.display.start < span.display.end);
        assert!(rendered.styled.text().is_char_boundary(span.display.start));
        assert!(rendered.styled.text().is_char_boundary(span.display.end));
        match &span.source {
            SourceMapping::Exact { original: range } => assert_eq!(
                original.get(range.clone()),
                rendered.styled.text().get(span.display.clone())
            ),
            SourceMapping::Transformed { original: range } => {
                assert!(range.start < range.end);
                assert!(original.get(range.clone()).is_some());
            }
            SourceMapping::Generated => {}
        }
        end = span.display.end;
    }
    assert_eq!(end, rendered.styled.text().len());
}

#[test]
fn primary_priority_precedes_actual_map_order_and_value_type() -> TestResult {
    let original = r#"{"response":"response first","written":"written second","content":"content third","text":42}"#;
    let rendered = render(original, false)?;
    assert_eq!(rendered.styled.text(), "42\n");
    exact_at(&rendered, original, "42")?;
    assert_mapping(&rendered, original);
    for (original, expected) in [
        (
            r#"{"response":"response","written":"written","content":"content"}"#,
            "content\n",
        ),
        (
            r#"{"response":"response","written":"written"}"#,
            "written\n",
        ),
        (r#"{"other":"other","response":"response"}"#, "response\n"),
    ] {
        assert_eq!(render(original, false)?.styled.text(), expected);
    }
    Ok(())
}

#[test]
fn fallback_and_secondary_fields_follow_the_actual_preserved_map_order() -> TestResult {
    let original = r#"{"z":1,"y":"chosen","a":"tail"}"#;
    let collapsed = render(original, false)?;
    assert_eq!(collapsed.styled.text(), "chosen\n");
    let expanded = render(original, true)?;
    assert_eq!(
        expanded.styled.text(),
        "chosen\n─── z ───\n1\n─── a ───\ntail\n"
    );
    exact_at(&expanded, original, "chosen")?;
    exact_at(&expanded, original, "tail")?;
    for heading in ["─── z", "─── a"] {
        assert_eq!(
            expanded.source_boundary(
                at(expanded.styled.text(), heading)?,
                BoundaryAffinity::After
            )?,
            SourceBoundary::Generated
        );
    }
    assert_mapping(&expanded, original);
    assert_eq!(
        render(r#"{"z":false,"a":null}"#, false)?.styled.text(),
        "false\n"
    );
    Ok(())
}

#[test]
fn decoded_markdown_retains_styles_and_original_exact_byte_offsets() -> TestResult {
    let original = r#"{"note":"hidden","text":"Hello **world** café"}"#;
    let rendered = render(original, false)?;
    assert_eq!(rendered.styled.text(), "Hello world café\n");
    for word in ["Hello", "world", "café"] {
        exact_at(&rendered, original, word)?;
    }
    let world = at(rendered.styled.text(), "world")?;
    assert!(
        rendered
            .styled
            .spans()
            .iter()
            .any(|span| span.range.contains(&world)
                && span.style.attributes.contains(TextAttribute::Bold))
    );
    assert_mapping(&rendered, original);
    Ok(())
}

#[test]
fn unicode_escapes_and_surrogate_pairs_keep_the_whole_raw_escape_interval() -> TestResult {
    let original = r#"{"text":"left \u00e9 \uD83D\uDE00 right","other":0}"#;
    let rendered = render(original, false)?;
    assert_eq!(rendered.styled.text(), "left é 😀 right\n");
    transformed_at(&rendered, original, "é", r"\u00e9")?;
    transformed_at(&rendered, original, "😀", r"\uD83D\uDE00")?;
    exact_at(&rendered, original, "right")?;
    assert_mapping(&rendered, original);
    Ok(())
}

#[test]
fn composed_markdown_transformation_covers_all_responsible_json_bytes() -> TestResult {
    let original = r#"{"text":"x \u0026amp; y","other":"hidden"}"#;
    let rendered = render(original, false)?;
    assert_eq!(rendered.styled.text(), "x & y\n");
    transformed_at(&rendered, original, "&", r"\u0026amp;")?;
    exact_at(&rendered, original, " y")?;
    assert_mapping(&rendered, original);
    Ok(())
}

#[test]
fn graphemes_crossing_exact_and_escape_runs_refuse_interior_source_positions() -> TestResult {
    let original = r#"{"text":"a\u0301 \uD83D\uDC69\u200d\uD83D\uDCBB","other":0}"#;
    let rendered = render(original, false)?;
    assert_eq!(rendered.styled.text(), "a\u{301} 👩‍💻\n");
    assert!(matches!(
        rendered.source_boundary(1, BoundaryAffinity::After),
        Err(MarkdownError::Boundary { offset: 1 })
    ));
    let emoji = at(rendered.styled.text(), "👩")?;
    assert!(matches!(
        rendered.source_boundary(emoji + '👩'.len_utf8(), BoundaryAffinity::After),
        Err(MarkdownError::Boundary { .. })
    ));
    exact_at(&rendered, original, "a")?;
    assert_mapping(&rendered, original);
    Ok(())
}

#[test]
fn json_controls_are_visible_data_and_keep_original_escape_provenance() -> TestResult {
    let original = r#"{"text":"safe\u001b[31m\u202eend","other":"hidden"}"#;
    let rendered = render(original, false)?;
    assert_eq!(rendered.styled.text(), "safe\\u{1b}[31m\\u{202e}end\n");
    assert!(!rendered.styled.text().contains('\u{1b}'));
    transformed_at(&rendered, original, r"\u{1b}", r"\u001b")?;
    transformed_at(&rendered, original, r"\u{202e}", r"\u202e")?;
    assert_mapping(&rendered, original);
    Ok(())
}

#[test]
fn every_json_escape_decodes_without_inventing_an_exact_inverse() -> TestResult {
    let original = r#""a\"b\\c\/d\be\ff\ng\rh\ti\u006aj\uD834\uDD1Ek""#;
    let decoded = decode_string(original, 0..original.len())?;
    assert_eq!(decoded.text, "a\"b\\c/d\u{8}e\u{c}f\ng\rh\tijj𝄞k");
    let raw_escapes = [
        r#"\""#,
        r"\\",
        r"\/",
        r"\b",
        r"\f",
        r"\n",
        r"\r",
        r"\t",
        r"\u006a",
        r"\uD834\uDD1E",
    ];
    let transformed: Vec<&str> = decoded
        .runs
        .iter()
        .filter(|run| !run.exact)
        .map(|run| {
            original
                .get(run.original.clone())
                .ok_or_else(|| std::io::Error::other("escape range invalid"))
        })
        .collect::<Result<_, _>>()?;
    assert_eq!(transformed, raw_escapes);
    for run in decoded.runs.iter().filter(|run| run.exact) {
        assert_eq!(
            decoded.text.get(run.decoded.clone()),
            original.get(run.original.clone())
        );
    }
    Ok(())
}

#[test]
fn code_field_preserves_tabs_quotes_and_slashes_as_transformed_json_escapes() -> TestResult {
    let original = r#"{"text":"```\nleft\t\"quoted\" \/\n```","other":0}"#;
    let rendered = render(original, false)?;
    assert_eq!(rendered.styled.text(), "left\t\"quoted\" /\n");
    transformed_at(&rendered, original, "\t", r"\t")?;
    transformed_at(&rendered, original, "\"", r#"\""#)?;
    transformed_at(&rendered, original, "/", r"\/")?;
    exact_at(&rendered, original, "quoted")?;
    assert_mapping(&rendered, original);
    Ok(())
}

#[test]
fn nested_nonstring_values_are_faithful_original_tokens_with_exact_ranges() -> TestResult {
    let original = r#"{"text":"answer","data": { "nested": [1, {"punctuation":"},:\""}], "number":1e2 },"flag":false}"#;
    let rendered = render(original, true)?;
    let raw = r#"{ "nested": [1, {"punctuation":"},:\""}], "number":1e2 }"#;
    assert!(rendered.styled.text().contains(raw));
    exact_at(&rendered, original, raw)?;
    exact_at(&rendered, original, "false")?;
    assert_mapping(&rendered, original);
    Ok(())
}

#[test]
fn duplicate_and_escaped_keys_use_the_last_matching_original_value() -> TestResult {
    let original = r#"{"text":"discarded","te\u0078t":"retained","other":"secondary"}"#;
    let rendered = render(original, true)?;
    assert_eq!(
        rendered.styled.text(),
        "retained\n─── other ───\nsecondary\n"
    );
    exact_at(&rendered, original, "retained")?;
    assert!(!rendered.styled.text().contains("discarded"));
    assert_mapping(&rendered, original);
    Ok(())
}

#[test]
fn generated_field_keys_cannot_emit_terminal_controls_or_claim_source_bytes() -> TestResult {
    let original = r#"{"text":"primary","\u001b[31m\nkey":"secondary"}"#;
    let rendered = render(original, true)?;
    assert!(!rendered.styled.text().contains('\u{1b}'));
    let key = at(rendered.styled.text(), r"\u{1b}")?;
    assert_eq!(
        rendered.source_boundary(key, BoundaryAffinity::After)?,
        SourceBoundary::Generated
    );
    exact_at(&rendered, original, "secondary")?;
    assert_mapping(&rendered, original);
    Ok(())
}

#[test]
fn partial_and_invalid_json_remain_original_under_explicit_unmapped_status() -> TestResult {
    for (original, status) in [
        (
            r#"{"text":"**unfinished"#,
            "Partial JSON-like text — original text\n",
        ),
        (
            r#"{"text":"\uD83D"#,
            "Partial JSON-like text — original text\n",
        ),
        (
            r#"{"text":"done", "tail": ["#,
            "Partial JSON-like text — original text\n",
        ),
        (r#"{"text": invalid}"#, "Invalid JSON-like text at line "),
    ] {
        for secondary in [false, true] {
            let rendered = render(original, secondary)?;
            assert!(rendered.styled.text().starts_with(status));
            assert!(rendered.styled.text().ends_with(original));
            assert_eq!(
                rendered.source_boundary(0, BoundaryAffinity::After)?,
                SourceBoundary::Generated
            );
            let body_start = rendered.styled.text().len() - original.len();
            assert_eq!(
                rendered.source_boundary(body_start, BoundaryAffinity::After)?,
                SourceBoundary::Exact { original_offset: 0 }
            );
            assert_mapping(&rendered, original);
        }
    }
    Ok(())
}

#[test]
fn nonobject_single_field_and_ordinary_prose_keep_the_original_markdown_view() -> TestResult {
    for original in [
        "plain **bold**",
        r#"{"text":"**single**"}"#,
        "[1, 2]",
        "{}",
        "",
    ] {
        let expected = render_markdown(original, highlighter())?;
        for secondary in [false, true] {
            let rendered = render(original, secondary)?;
            assert_eq!(rendered.styled.text(), expected.styled.text());
            assert_eq!(rendered.styled.spans(), expected.styled.spans());
            assert_eq!(rendered.spans, expected.spans);
        }
    }
    Ok(())
}

#[test]
fn leading_links_lists_and_brace_prose_do_not_invent_a_json_contract() -> TestResult {
    for original in [
        "[docs](https://example.com)",
        "[x] task with **emphasis**",
        "[ ] another task",
        "- [x] completed\n- [ ] remaining",
        "[reference][docs]\n\n[docs]: https://example.com",
        "[**bold label**](https://example.com)",
        "[unfinished **link",
        "[1, **two**",
        "{note} **prose**",
        r#"{ "unfinished **key"#,
        r#"{ "quoted key" } **prose**"#,
    ] {
        let expected = render_markdown(original, highlighter())?;
        for secondary in [false, true] {
            let rendered = render(original, secondary)?;
            assert_eq!(rendered.styled.text(), expected.styled.text());
            assert_eq!(rendered.styled.spans(), expected.styled.spans());
            assert_eq!(rendered.spans, expected.spans);
            assert_mapping(&rendered, original);
        }
    }
    let link = render("[docs](https://example.com)", false)?;
    assert_eq!(link.styled.text(), "docs (https://example.com)");
    assert!(
        link.styled
            .spans()
            .iter()
            .any(|span| span.style.attributes.contains(TextAttribute::Underline))
    );
    Ok(())
}

#[test]
fn partial_object_recognition_respects_whitespace_and_escaped_key_quotes() -> TestResult {
    for original in [
        " \n{ \t\"text\" \n: \"**unfinished",
        r#"{"quoted\"key": "**unfinished"#,
    ] {
        let rendered = render(original, false)?;
        assert!(
            rendered
                .styled
                .text()
                .starts_with("Partial JSON-like text — original text\n")
        );
        assert!(rendered.styled.text().ends_with(original));
        assert_mapping(&rendered, original);
    }
    Ok(())
}

#[test]
fn empty_fields_and_existing_hard_newlines_do_not_gain_duplicate_blank_lines() -> TestResult {
    for (original, expected) in [
        (r#"{"text":"","other":"tail"}"#, "\n─── other ───\ntail\n"),
        (r#"{"text":"line\n","other":""}"#, "line\n─── other ───\n"),
    ] {
        let rendered = render(original, true)?;
        assert_eq!(rendered.styled.text(), expected);
        assert_mapping(&rendered, original);
    }
    Ok(())
}
