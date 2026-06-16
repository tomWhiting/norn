//! Dim-stream rendering for text and thinking deltas.
//!
//! Extracted from `dispatch.rs` to keep that module under the 500-line
//! production code limit. Owns the streaming text path: dim preview
//! writes, the styled line-pop replacement, and reasoning-summary
//! buffering.

use std::io::Write as _;

use termina::Terminal as _;

use crate::TuiError;
use crate::render::MarkdownRenderer;
use crate::render::markdown::FeedOutput;
use crate::render::scroll_region::write_to_scroll;
use crate::render::thinking::render_thinking;
use crate::terminal::setup::TerminalGuard;

use super::helpers::{
    clear_dim_state, dim_line_count, erase_dim_lines, flush_terminal, sync_with_guard,
};
use super::state::AppState;

/// Render and finish any pending reasoning summary before another
/// scroll-region surface writes normal content.
///
/// Reasoning summary chunks are buffered so heading-aware block
/// formatting can be decided from the complete summary. Rendering here
/// keeps answer text, tool output, or final status below the thought
/// block in normal scrollback order.
pub(super) fn finish_thinking_block(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: &mut Option<MarkdownRenderer>,
) -> Result<(), TuiError> {
    if state.thinking_buffer.is_empty() {
        return Ok(());
    }
    clear_dim_state(state, guard, renderer.as_mut())?;
    let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
    let thought_block = render_thinking(&state.thinking_buffer, &state.terminal_caps, cols);
    write_to_scroll(&thought_block, guard.terminal_mut())?;
    guard.note_scroll_newlines(&thought_block)?;
    state.thinking_buffer.clear();
    Ok(())
}

/// Accept a `TextDelta`: feed through the streaming
/// [`MarkdownRenderer`] and write dim preview + styled output.
///
/// Dim-stream: every token is written in dim SGR immediately for
/// streaming feel. When a complete line arrives (`\n`), the dim text
/// is cleared (CR + erase-in-line) and replaced with the fully
/// styled version — the "line-pop" effect. Inside code fences, dim
/// preview is suppressed; the entire block is written styled.
pub(super) fn handle_text_delta(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    renderer: &mut Option<MarkdownRenderer>,
    text: &str,
) -> Result<(), TuiError> {
    if text.is_empty() {
        return Ok(());
    }
    finish_thinking_block(state, guard, renderer)?;
    if state.last_was_tool_result {
        write_to_scroll("\n", guard.terminal_mut())?;
        guard.note_scroll_newlines("\n")?;
        state.last_was_tool_result = false;
    }
    state.est_output_bytes = state.est_output_bytes.saturating_add(text.len());
    state.current_tool_use = None;
    let r = if let Some(r) = renderer {
        r
    } else {
        let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
        renderer.insert(MarkdownRenderer::new(state.terminal_caps.clone(), cols))
    };
    let output = r.feed(text);
    let wrote;
    if output.replace_dim {
        // Composite frame: erase the live dim → write the styled
        // replacement → optionally write the next dim preview. Wrap
        // in DCS 2026 brackets (cursor hide/show on baseline
        // terminals) so the terminal paints the line-pop atomically
        // and the user never sees the cleared-but-not-yet-rewritten
        // intermediate state. R11.
        let caps = state.terminal_caps.clone();
        sync_with_guard(&caps, guard, |guard| {
            erase_dim_lines(state.dim_wrapped_lines, guard)?;
            state.dim_wrapped_lines = 0;
            write_styled_then_dim(state, guard, &output)?;
            Ok(())
        })?;
        wrote = true;
    } else {
        // No prior dim to replace — styled and dim are written
        // individually. Per brief, individual dim writes are NOT
        // wrapped in sync brackets (the overhead of the bracket pair
        // would exceed the render time for a single short write).
        wrote = write_styled_then_dim(state, guard, &output)?;
    }
    if wrote {
        flush_terminal(guard)?;
    }
    state.text_streamed_this_turn = true;
    Ok(())
}

/// Emit the styled segment (if any), insert a separator newline when a
/// new dim preview needs its own line below mid-line styled output,
/// then write the dim preview wrapped in dim SGR. Shared between the
/// sync-bracketed `replace_dim` path and the unwrapped append path so
/// the line-pop logic stays in lockstep. Returns whether any bytes
/// were written so the caller can gate `flush_terminal`.
fn write_styled_then_dim(
    state: &mut AppState,
    guard: &mut TerminalGuard,
    output: &FeedOutput,
) -> Result<bool, TuiError> {
    let mut wrote = false;
    if !output.styled.is_empty() {
        write_to_scroll(&output.styled, guard.terminal_mut())?;
        guard.note_scroll_newlines(&output.styled)?;
        state.styled_mid_line = !output.styled.ends_with('\n');
        wrote = true;
    }
    if !output.dim.is_empty() && state.styled_mid_line {
        write_to_scroll("\n", guard.terminal_mut())?;
        guard.note_scroll_newlines("\n")?;
        state.styled_mid_line = false;
    }
    if !output.dim.is_empty() {
        let cols = guard.terminal_mut().get_dimensions().map_or(80, |d| d.cols);
        let lines = dim_line_count(&output.dim, cols);
        let avail = guard.scroll_rows_below_cursor();
        if lines <= avail {
            let writer = guard.terminal_mut();
            write!(writer, "\x1b[2m")?;
            write_to_scroll(&output.dim, writer)?;
            write!(writer, "\x1b[22m")?;
            state.dim_wrapped_lines = lines;
            wrote = true;
        }
    }
    Ok(wrote)
}

/// Accept a `ThinkingDelta`: buffer it when thinking is visible,
/// otherwise discard it.
///
/// The tokens are spent regardless of whether the bytes are painted, so
/// [`AppState::est_output_bytes`] accumulates either way. `ThinkingDelta`
/// also signals the model has moved past any pending tool call (it's
/// processing, not executing), so [`AppState::current_tool_use`] is
/// cleared.
pub(super) fn handle_thinking_delta(state: &mut AppState, text: &str) {
    state.est_output_bytes = state.est_output_bytes.saturating_add(text.len());
    state.current_tool_use = None;
    if text.is_empty() || !state.display_toggles.thinking_visible {
        return;
    }
    state.thinking_buffer.push_str(text);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::terminal::caps::TerminalCaps;

    #[test]
    fn text_delta_pipeline_writes_hello_to_writer() {
        let caps = TerminalCaps::baseline();
        let mut renderer = MarkdownRenderer::new(caps, 80);
        let mut buf: Vec<u8> = Vec::new();
        let output = renderer.feed("hello");
        if !output.dim.is_empty() {
            crate::render::scroll_region::write_to_scroll(&output.dim, &mut buf).unwrap();
        }
        if !output.styled.is_empty() {
            crate::render::scroll_region::write_to_scroll(&output.styled, &mut buf).unwrap();
        }
        let tail = renderer.finalize();
        if !tail.styled.is_empty() {
            crate::render::scroll_region::write_to_scroll(&tail.styled, &mut buf).unwrap();
        }
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("hello"), "got: {out:?}");
    }
}
