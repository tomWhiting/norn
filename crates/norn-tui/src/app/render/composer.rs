//! Full-width grapheme composer and on-demand completion overlay preparation.

use super::{
    AppState, Arc, DisplayText, Focus, Frame, PaintRow, Rect, RenderedMarkdown, TextRow, TuiError,
    interaction, push_text,
};
use std::fmt::Write as _;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(super) fn paint_chrome(
    state: &AppState,
    frame: &mut Frame,
    panel: Rect,
) -> Result<(), TuiError> {
    if panel.height <= crate::render::layout::COMPOSER_CHROME_ROWS {
        return Ok(());
    }
    let status = state.fixed_panel.status_bar();
    let mode = state.in_flight_input.mode().label();
    let mut input = format!(
        "{mode} • {}↑ {}↓",
        crate::render::text::format_count(status.input_tokens),
        crate::render::text::format_count(status.output_tokens)
    );
    if state.in_flight_input.is_running()
        && let Some(start) = state.turn_start
    {
        write!(input, " • {}s", start.elapsed().as_secs()).map_err(interaction)?;
    }
    let mut metadata = vec![status.model_name.clone()];
    if let Some(tier) = &status.service_tier {
        metadata.push(format!("tier:{tier}"));
    }
    if let Some(effort) = &status.reasoning_effort {
        metadata.push(format!("effort:{effort}"));
    }
    if !status.session_name.is_empty() {
        metadata.push(status.session_name.clone());
    }
    for (row, label, right) in [
        (panel.row, input, false),
        (panel.row + panel.height - 2, metadata.join(" • "), true),
    ] {
        let chip = format!("🮠 {label} 🮣");
        let padding = usize::from(panel.width).saturating_sub(chip.width() + 3);
        let line = if right {
            format!("{}{}───", "─".repeat(padding), chip)
        } else {
            format!("───{}{}", chip, "─".repeat(padding))
        };
        chrome_line(
            frame,
            &line,
            Rect {
                row,
                height: 1,
                ..panel
            },
        )?;
    }
    chrome_line(
        frame,
        &format!(
            "{}  ^O verbose  ^E thinking  ^T {}  {}",
            status.key_hints,
            if mode == "steer" { "queue" } else { "steer" },
            crate::render::style::newline_key_hint(&state.terminal_caps)
        ),
        Rect {
            row: panel.row + panel.height - 1,
            height: 1,
            ..panel
        },
    )
}

fn chrome_line(frame: &mut Frame, line: &str, area: Rect) -> Result<(), TuiError> {
    let line = crate::render::text::truncate_with_ellipsis(line, area.width);
    let mut text = crate::render::retained_markdown::render_plain(&line)?;
    let displayed = text.styled.text().to_owned();
    let span = crate::render::retained_text::StyleSpan {
        range: 0..displayed.len(),
        style: crate::render::retained_text::TextStyle {
            attributes: crate::render::retained_text::TextAttributes::default()
                .with(crate::render::retained_text::TextAttribute::Dim),
            ..Default::default()
        },
    };
    text.styled = crate::render::retained_text::StyledText::new(displayed, vec![span])?;
    if let Some(geometry) = super::layout_rows(&text.styled, area.width)?
        .into_iter()
        .next()
    {
        frame.rows.push(PaintRow {
            area,
            row: 0,
            text: Arc::new(text),
            geometry,
            selected: false,
            selection: Vec::new(),
            composer: false,
        });
    }
    Ok(())
}

pub(super) fn paint_composer(
    state: &AppState,
    frame: &mut Frame,
    composer: Rect,
    prefix: u16,
    draft: &Arc<RenderedMarkdown>,
    rows: &[TextRow],
    original_cursor: usize,
) -> Result<(), TuiError> {
    let original = state.input_editor.text();
    let snapped = original
        .grapheme_indices(true)
        .find_map(|(offset, grapheme)| {
            (offset <= original_cursor && original_cursor < offset + grapheme.len())
                .then_some(offset)
        })
        .unwrap_or(original_cursor);
    let cursor = DisplayText::new(&original[..snapped]).as_str().len();
    let cursor_row = rows
        .iter()
        .rposition(|row| {
            let bytes = row.bytes();
            bytes.start <= cursor && cursor <= bytes.end
        })
        .unwrap_or(0);
    let first = cursor_row.saturating_sub(usize::from(composer.height).saturating_sub(1));
    let input = Rect {
        column: prefix,
        width: composer.width.saturating_sub(prefix),
        ..composer
    };
    for (index, geometry) in rows
        .iter()
        .skip(first)
        .take(usize::from(composer.height))
        .enumerate()
    {
        frame.rows.push(PaintRow {
            area: input,
            row: u16::try_from(index).map_err(|source| TuiError::FrameCoordinate {
                value: index,
                source,
            })?,
            text: Arc::clone(draft),
            geometry: geometry.clone(),
            selected: false,
            selection: Vec::new(),
            composer: true,
        });
    }
    if prefix > 0 {
        push_text(
            frame,
            "› ",
            Rect {
                width: prefix,
                height: 1,
                ..composer
            },
            false,
            true,
        )?;
    }
    if state
        .screen
        .focus
        .visible(state.screen.availability())
        .map_err(interaction)?
        == Focus::Composer
        && let Some(row) = rows.get(cursor_row)
    {
        let column = row
            .column_for(cursor)?
            .min(usize::from(input.width.saturating_sub(1)));
        frame.cursor = Some((
            input.column
                + u16::try_from(column).map_err(|source| TuiError::FrameCoordinate {
                    value: column,
                    source,
                })?,
            composer.row
                + u16::try_from(cursor_row - first).map_err(|source| {
                    TuiError::FrameCoordinate {
                        value: cursor_row - first,
                        source,
                    }
                })?,
        ));
    }
    Ok(())
}

pub(super) fn popup(state: &AppState, frame: &mut Frame, composer: Rect) -> Result<(), TuiError> {
    let Some(popup) = &state.autocomplete else {
        return Ok(());
    };
    let height = popup.height().min(composer.row);
    let area = Rect {
        column: 0,
        row: composer.row - height,
        width: composer.width,
        height,
    };
    for (index, candidate) in popup
        .candidates
        .iter()
        .enumerate()
        .skip(popup.visible_offset)
        .take(usize::from(height))
    {
        let text = match candidate {
            crate::input::autocomplete::CandidateRow::Slash(row) => {
                format!("/{}  {}", row.name, row.description)
            }
            crate::input::autocomplete::CandidateRow::File(row) => row.path.clone(),
        };
        let row = u16::try_from(index - popup.visible_offset).map_err(|source| {
            TuiError::FrameCoordinate {
                value: index - popup.visible_offset,
                source,
            }
        })?;
        push_text(
            frame,
            &text,
            Rect {
                row: area.row + row,
                height: 1,
                ..area
            },
            index == popup.selected_index,
            true,
        )?;
    }
    Ok(())
}

pub(super) fn input_text(content: &str) -> Result<Arc<RenderedMarkdown>, TuiError> {
    let mut rendered = crate::render::retained_markdown::render_plain(content)?;
    let text = rendered.styled.text().to_owned();
    let end = text.find('\n').unwrap_or(text.len());
    let spans = if end == 0 {
        Vec::new()
    } else {
        vec![crate::render::retained_text::StyleSpan {
            range: 0..end,
            style: crate::render::retained_text::TextStyle {
                foreground: Some([80, 160, 220]),
                ..Default::default()
            },
        }]
    };
    rendered.styled = crate::render::retained_text::StyledText::new(text, spans)?;
    Ok(Arc::new(rendered))
}

pub(super) fn input_margin(
    frame: &mut Frame,
    area: Rect,
    index: usize,
    first: bool,
) -> Result<Rect, TuiError> {
    let margin = area.width.min(2);
    if first && margin > 0 {
        let prefix = input_text("> ")?;
        if let Some(geometry) = super::layout_rows(&prefix.styled, margin)?
            .into_iter()
            .next()
        {
            frame.rows.push(PaintRow {
                area: Rect {
                    width: margin,
                    ..area
                },
                row: u16::try_from(index).map_err(|source| TuiError::FrameCoordinate {
                    value: index,
                    source,
                })?,
                text: prefix,
                geometry,
                selected: false,
                selection: Vec::new(),
                composer: false,
            });
        }
    }
    Ok(Rect {
        column: area.column + margin,
        width: area.width - margin,
        ..area
    })
}

pub(super) fn activity_status(state: &AppState) -> Option<String> {
    use crate::render::streaming_indicator::StreamingIndicator;
    if let Some(feedback) = &state.screen.feedback {
        return Some(feedback.clone());
    }
    let model = &state.fixed_panel.status_bar().model_name;
    let activity = match &state.streaming_indicator {
        StreamingIndicator::Idle => return state.in_flight_input.status_line(),
        StreamingIndicator::Generating {
            elapsed,
            est_output_tokens,
            ..
        } => format!(
            "{model} · generating {}s · ~{est_output_tokens} output tokens",
            elapsed.as_secs()
        ),
        StreamingIndicator::Retrying {
            attempt,
            max_attempts,
            error_class,
            remaining,
            ..
        } => format!(
            "{model} · retry {attempt}/{} in {}s · {error_class}",
            max_attempts.map_or_else(|| "unbounded".to_owned(), |value| value.to_string()),
            remaining.as_secs()
        ),
        StreamingIndicator::Complete { .. } => return None,
    };
    Some(match state.in_flight_input.status_line() {
        Some(input) => format!("{activity} · {input}"),
        None => activity,
    })
}
