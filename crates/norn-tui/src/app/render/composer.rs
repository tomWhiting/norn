//! Full-width grapheme composer and on-demand completion overlay preparation.

use super::{
    AppState, Arc, Frame, PaintRow, Rect, RenderedMarkdown, TuiError, interaction, push_text,
};
use std::fmt::Write as _;
use unicode_width::UnicodeWidthStr;

pub(super) fn paint_chrome(
    state: &mut AppState,
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
    let button = match state.composer_send_key {
        crate::frontend_preferences::ComposerSendKey::Enter => "[Enter sends]",
        crate::frontend_preferences::ComposerSendKey::ShiftEnter => {
            if state.terminal_caps.kitty_keyboard {
                "[Shift+Enter sends]"
            } else {
                "[Shift+Enter unconfirmed]"
            }
        }
        crate::frontend_preferences::ComposerSendKey::AltEnter => "[Alt+Enter sends]",
    };
    let newline = match state.composer_send_key {
        crate::frontend_preferences::ComposerSendKey::Enter => {
            crate::render::style::newline_key_hint(&state.terminal_caps)
        }
        crate::frontend_preferences::ComposerSendKey::ShiftEnter
        | crate::frontend_preferences::ComposerSendKey::AltEnter => "Enter",
    };
    let last_row = panel.row + panel.height - 1;
    let button_width =
        u16::try_from(button.width()).map_err(|source| TuiError::FrameCoordinate {
            value: button.width(),
            source,
        })?;
    let send_shortcut = state
        .view_shortcuts
        .hint(crate::input::view_shortcuts::ViewAction::SendKeyCycle);
    let hints = format!(
        "{button}  {send_shortcut} send key  {newline} newline  {}  ^O verbose  ^E thinking  ^T {}",
        status.key_hints,
        if mode == "steer" { "queue" } else { "steer" }
    );
    let latest = crate::app::view_actions::latest::LABEL;
    let latest_width =
        u16::try_from(latest.width()).map_err(|source| TuiError::FrameCoordinate {
            value: latest.width(),
            source,
        })?;
    let show_latest = (!state.screen.viewport.follows_tail() || state.transcript.latest_pending())
        && latest_width <= panel.width;
    let hints_width = if show_latest {
        panel.width.saturating_sub(latest_width).saturating_sub(1)
    } else {
        panel.width
    };
    let hints = crate::render::text::truncate_with_ellipsis(&hints, hints_width);
    state.screen.composer_send_key_area = hints.starts_with(button).then_some(Rect {
        column: panel.column,
        row: last_row,
        width: button_width,
        height: 1,
    });
    if show_latest {
        let area = Rect {
            column: panel.column + panel.width - latest_width,
            row: last_row,
            width: latest_width,
            height: 1,
        };
        chrome_line(frame, latest, area)?;
        state.screen.prepared_latest = Some(area);
    }
    chrome_line(
        frame,
        &hints,
        Rect {
            row: last_row,
            height: 1,
            width: hints_width,
            ..panel
        },
    )
}

fn chrome_line(frame: &mut Frame, line: &str, area: Rect) -> Result<(), TuiError> {
    let line = crate::render::text::truncate_with_ellipsis(line, area.width);
    if line.is_empty() {
        return Ok(());
    }
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
    let end = text.len();
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

#[cfg(test)]
#[path = "composer_tests.rs"]
mod tests;
