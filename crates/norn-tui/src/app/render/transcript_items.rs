//! Cached semantic item presentation with original source maps and generated local labels.

use super::{DisplayCache, ScreenState, interaction, layout_rows, safe_text};
use crate::TuiError;
use crate::app::transcript::Transcript;
use crate::render::retained_markdown::{
    RenderedMarkdown, SourceMapping, render_markdown, render_plain,
};
use crate::render::retained_text::{
    StyleSpan, StyledText, TextAttribute, TextAttributes, TextRow, TextStyle,
};
use norn::session_view::{BodyRef, ToolState, ViewItem, ViewItemKind};
use std::sync::Arc;

pub(super) struct RowGroup {
    pub(super) text: Arc<RenderedMarkdown>,
    pub(super) rows: Arc<[TextRow]>,
    pub(super) reference: Option<BodyRef>,
    pub(super) fixed_offset: Option<usize>,
    pub(super) before_item: bool,
}

pub(super) fn item_groups(
    transcript: &Transcript,
    screen: &mut ScreenState,
    item: &ViewItem,
    columns: u16,
    secondary_fields: bool,
    separator: bool,
) -> Result<Vec<RowGroup>, TuiError> {
    let expanded = screen
        .tool_overrides
        .get(&item.id)
        .copied()
        .unwrap_or(transcript.config.expanded_tools);
    let label = match &item.kind {
        ViewItemKind::Tool(tool) => crate::app::tool_calls::label(tool, expanded),
        ViewItemKind::Input | ViewItemKind::Text | ViewItemKind::Structured => String::new(),
        _ => item.label.as_str().to_owned(),
    };
    let mut groups = Vec::new();
    if separator {
        let mut blank = local_group("", columns, None)?;
        blank.before_item = true;
        groups.push(blank);
    }
    if !label.is_empty() {
        let mut header = local_group(&label, columns, None)?;
        header.text = Arc::new(header_text(&label, &item.kind)?);
        groups.push(header);
    }
    if (matches!(&item.kind, ViewItemKind::Tool(_)) && !expanded)
        || (transcript.completion_compact(&item.id)
            && !screen
                .tool_overrides
                .get(&item.id)
                .copied()
                .unwrap_or(false)
            && screen.viewport.selected() != Some(&item.id))
    {
        return Ok(groups);
    }
    for reference in &item.bodies {
        let Some(body) = transcript.body(reference) else {
            groups.push(local_group(
                "[content not loaded]",
                columns,
                Some(reference.clone()),
            )?);
            continue;
        };
        let length = body.original.len();
        let content =
            crate::app::streaming::complete_prefix(&body.original, body.next_offset.is_some());
        let cache = match screen.displayed.entry(reference.clone()) {
            std::collections::hash_map::Entry::Occupied(entry)
                if entry.get().original_len == length
                    && entry.get().secondary_fields == secondary_fields =>
            {
                entry.into_mut()
            }
            entry => {
                let text = if matches!(item.kind, ViewItemKind::Text | ViewItemKind::Structured) {
                    Arc::new(
                        crate::render::retained_structured::render_structured(
                            content,
                            secondary_fields,
                            &screen.highlighter,
                        )
                        .map_err(interaction)?,
                    )
                } else if matches!(item.kind, ViewItemKind::Thinking) {
                    Arc::new(render_markdown(content, &screen.highlighter)?)
                } else if matches!(item.kind, ViewItemKind::Input) {
                    super::composer::input_text(content)?
                } else {
                    safe_text(content)?
                };
                let base = body_style(&item.kind);
                let text = if base == TextStyle::default() {
                    text
                } else {
                    Arc::new(with_base_style(Arc::unwrap_or_clone(text), base)?)
                };
                let rows = Arc::from(layout_rows(&text.styled, columns)?);
                let cache = DisplayCache {
                    original_len: length,
                    secondary_fields,
                    text,
                    columns,
                    rows,
                };
                match entry {
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        entry.insert(cache);
                        entry.into_mut()
                    }
                    std::collections::hash_map::Entry::Vacant(entry) => entry.insert(cache),
                }
            }
        };
        if cache.columns != columns {
            cache.rows = Arc::from(layout_rows(&cache.text.styled, columns)?);
            cache.columns = columns;
        }
        groups.push(RowGroup {
            text: Arc::clone(&cache.text),
            rows: Arc::clone(&cache.rows),
            reference: Some(reference.clone()),
            fixed_offset: None,
            before_item: false,
        });
        if body.next_offset.is_some() {
            let mut more = local_group(
                "[more content available: /view more]",
                columns,
                Some(reference.clone()),
            )?;
            more.fixed_offset = Some(body.original.len());
            groups.push(more);
        }
    }
    Ok(groups)
}

fn local_group(
    label: &str,
    columns: u16,
    reference: Option<BodyRef>,
) -> Result<RowGroup, TuiError> {
    let mut rendered = render_plain(label)?;
    for span in &mut rendered.spans {
        span.source = SourceMapping::Generated;
    }
    let text = Arc::new(rendered);
    let rows: Vec<_> = layout_rows(&text.styled, columns)?
        .into_iter()
        .take(1)
        .collect();
    Ok(RowGroup {
        text,
        rows: Arc::from(rows),
        before_item: false,
        fixed_offset: reference.as_ref().map(|_| 0),
        reference,
    })
}

// Existing Norn tool/error palette from tools/helpers.rs, expressed as typed spans.
const ERROR_RED: [u8; 3] = [200, 80, 80];
const WARNING_AMBER: [u8; 3] = [215, 175, 0];

fn body_style(kind: &ViewItemKind) -> TextStyle {
    let dim = TextAttributes::default().with(TextAttribute::Dim);
    match kind {
        ViewItemKind::Thinking => TextStyle {
            attributes: dim.with(TextAttribute::Italic),
            ..TextStyle::default()
        },
        ViewItemKind::Error | ViewItemKind::Refusal => TextStyle {
            foreground: Some(ERROR_RED),
            ..TextStyle::default()
        },
        ViewItemKind::Tool(_)
        | ViewItemKind::Notice
        | ViewItemKind::Metadata
        | ViewItemKind::Context
        | ViewItemKind::ModelChange { .. } => TextStyle {
            attributes: dim,
            ..TextStyle::default()
        },
        _ => TextStyle::default(),
    }
}

fn tool_colour(tool: &norn::session_view::ToolView) -> Option<[u8; 3]> {
    let states = [Some(tool.state), tool.result_state];
    if states.contains(&Some(ToolState::Failed)) {
        Some(ERROR_RED)
    } else if states
        .iter()
        .any(|state| matches!(state, Some(ToolState::Blocked | ToolState::Incomplete)))
    {
        Some(WARNING_AMBER)
    } else {
        None
    }
}

fn header_text(label: &str, kind: &ViewItemKind) -> Result<RenderedMarkdown, TuiError> {
    let mut base = body_style(kind);
    if matches!(kind, ViewItemKind::Child | ViewItemKind::ExternalInput) {
        base.attributes = base.attributes.with(TextAttribute::Bold);
    }
    let mut rendered = render_plain(label)?;
    for span in &mut rendered.spans {
        span.source = SourceMapping::Generated;
    }
    let name_end = if let ViewItemKind::Tool(tool) = kind {
        base.foreground = tool_colour(tool);
        crate::tools::summary::summarize(tool, false)
            .name_label()
            .len()
    } else {
        0
    };
    let mut rendered = with_base_style(rendered, base)?;
    if name_end > 0 {
        let text = rendered.styled.text().to_owned();
        let mut spans = vec![StyleSpan {
            range: 0..name_end,
            style: TextStyle {
                attributes: base.attributes.with(TextAttribute::Bold),
                ..base
            },
        }];
        if name_end < text.len() {
            spans.push(StyleSpan {
                range: name_end..text.len(),
                style: base,
            });
        }
        rendered.styled = StyledText::new(text, spans)?;
    }
    Ok(rendered)
}

fn with_base_style(
    mut rendered: RenderedMarkdown,
    base: TextStyle,
) -> Result<RenderedMarkdown, TuiError> {
    if base == TextStyle::default() {
        return Ok(rendered);
    }
    let text = rendered.styled.text().to_owned();
    let mut spans = Vec::new();
    let mut end = 0;
    for span in rendered.styled.spans() {
        if end < span.range.start {
            spans.push(StyleSpan {
                range: end..span.range.start,
                style: base,
            });
        }
        let mut style = span.style;
        style.foreground = style.foreground.or(base.foreground);
        style.background = style.background.or(base.background);
        for attribute in [
            TextAttribute::Bold,
            TextAttribute::Dim,
            TextAttribute::Italic,
            TextAttribute::Underline,
            TextAttribute::Strike,
        ] {
            if base.attributes.contains(attribute) {
                style.attributes = style.attributes.with(attribute);
            }
        }
        spans.push(StyleSpan {
            range: span.range.clone(),
            style,
        });
        end = span.range.end;
    }
    if end < text.len() {
        spans.push(StyleSpan {
            range: end..text.len(),
            style: base,
        });
    }
    rendered.styled = StyledText::new(text, spans)?;
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use norn::session_view::{DisplayText, ToolView};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn tool() -> ToolView {
        ToolView {
            call_id: Some("presentation-call".to_owned()),
            stream_item_id: None,
            name: Some(DisplayText::new("read")),
            description: Some(DisplayText::new(
                "Inspect the word failed without treating it as an outcome",
            )),
            description_error: None,
            kind: None,
            arguments: None,
            result: None,
            invocation_event: None,
            invocation_attempt: None,
            result_event: None,
            result_parent: None,
            state: ToolState::Running,
            result_state: None,
            duration_ms: None,
            committed: None,
        }
    }

    #[test]
    fn thinking_styles_preserve_exact_mapping_markdown_emphasis_and_code_colour() -> TestResult {
        let original = "Reason **carefully** about `let α = 1;`";
        let rendered = render_markdown(original, &crate::render::syntax::SyntaxHighlighter::new())?;
        let before = rendered.clone();
        let styled = with_base_style(rendered, body_style(&ViewItemKind::Thinking))?;
        assert_eq!(styled.styled.text(), before.styled.text());
        assert_eq!(styled.spans, before.spans);
        assert!(styled.styled.spans().iter().all(|span| {
            span.style.attributes.contains(TextAttribute::Dim)
                && span.style.attributes.contains(TextAttribute::Italic)
        }));
        for span in before.styled.spans() {
            let actual = styled
                .styled
                .spans()
                .iter()
                .find(|current| current.range == span.range)
                .ok_or("styled source span missing")?;
            assert_eq!(actual.style.foreground, span.style.foreground);
            if span.style.attributes.contains(TextAttribute::Bold) {
                assert!(actual.style.attributes.contains(TextAttribute::Bold));
            }
        }
        Ok(())
    }

    #[test]
    fn tool_styles_use_typed_outcomes_and_generated_labels_keep_no_original_authority() -> TestResult
    {
        let mut tool = tool();
        assert_eq!(tool_colour(&tool), None);
        tool.result_state = Some(ToolState::Blocked);
        assert_eq!(tool_colour(&tool), Some(WARNING_AMBER));
        tool.state = ToolState::Failed;
        assert_eq!(tool_colour(&tool), Some(ERROR_RED));
        let label = crate::tools::summary::summarize(&tool, false).header();
        let rendered = header_text(&label, &ViewItemKind::Tool(Box::new(tool)))?;
        assert_eq!(rendered.styled.text(), label);
        assert!(
            rendered
                .spans
                .iter()
                .all(|span| span.source == SourceMapping::Generated)
        );
        let first = rendered
            .styled
            .spans()
            .first()
            .ok_or("tool name style absent")?;
        assert_eq!(first.range, 0.."read".len());
        assert!(first.style.attributes.contains(TextAttribute::Bold));
        assert!(
            rendered
                .styled
                .spans()
                .iter()
                .all(|span| span.style.foreground == Some(ERROR_RED))
        );
        Ok(())
    }

    #[test]
    fn generated_separator_has_no_body_capability_or_mapped_bytes() -> TestResult {
        let group = local_group("", 80, None)?;
        assert!(group.reference.is_none());
        assert!(group.fixed_offset.is_none());
        assert!(group.text.spans.is_empty());
        assert_eq!(group.rows.len(), 1);
        assert!(group.rows[0].bytes().is_empty());
        Ok(())
    }
}
