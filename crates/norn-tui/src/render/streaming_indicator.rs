//! The streaming status row's state machine.
//!
//! One enum, [`StreamingIndicator`], carrying what the row above the
//! session-metadata divider is currently saying: nothing, live
//! generation, a provider-retry wait, or the finished turn's usage
//! summary. [`super::fixed_panel`] composes it into the panel; the state
//! itself, its repaint key, and its rendering live here so the
//! compositor stays a compositor.

use std::io;
use std::time::{Duration, Instant};

use termina::OneBased;
use termina::escape::csi::{Csi, Cursor, Edit, EraseInLine, Sgr};
use termina::style::RgbColor;
use unicode_width::UnicodeWidthStr as _;

use super::retry_status::{retry_repaint_key, retry_row_body};
use super::style::colour_for;
use super::text::{format_count, truncate_with_ellipsis};
use crate::terminal::caps::TerminalCaps;

/// Foreground colour for the streaming indicator's `generating` text.
/// Shared with the retry-wait row: both are "work in flight", and the
/// retry row must not read as a terminal failure.
const GENERATING_COLOUR: RgbColor = RgbColor::new(215, 175, 0);

/// Cursor-position escape targeting the start of a zero-based `row`.
fn cursor_to(row: u16) -> Csi {
    Csi::Cursor(Cursor::Position {
        line: OneBased::from_zero_based(row),
        col: OneBased::from_zero_based(0),
    })
}

/// Escape that erases the entire line the cursor sits on.
fn erase_line() -> Csi {
    Csi::Edit(Edit::EraseInLine(EraseInLine::EraseLine))
}

/// Tool call the assistant is currently executing, surfaced on the
/// streaming indicator's `Generating` mode while a result is pending.
///
/// `description` is the model-supplied `tool_use_description` envelope
/// field (see [`norn::tool::envelope::ENVELOPE_DESCRIPTION_KEY`]); it
/// arrives only at `ToolCallComplete`, so during the gap between the
/// first `ToolCallDelta` (which carries the name) and `ToolCallComplete`
/// the renderer paints the tool name alone.
#[derive(Clone, Debug)]
pub struct ToolUseInFlight {
    /// Name of the tool being invoked.
    pub tool_name: String,
    /// Model-supplied intent description. `None` when not yet available
    /// or when the model failed to populate the envelope field.
    pub description: Option<String>,
}

/// State of the streaming indicator row.
#[derive(Clone, Debug, Default)]
pub enum StreamingIndicator {
    /// The model is not producing output — the row is absent (0 rows).
    #[default]
    Idle,
    /// The model is producing output — shows elapsed time, an estimated
    /// running output-token count, and (when in flight) the active
    /// tool's name and description (1 row).
    Generating {
        /// Time elapsed since generation began.
        elapsed: Duration,
        /// Output-token estimate accumulated by the dispatch layer —
        /// `bytes / 4` heuristic over `TextDelta`, `ThinkingDelta`, and
        /// `ToolCallDelta` content. Displayed with a `~` prefix to
        /// advertise the approximation.
        est_output_tokens: u64,
        /// Tool call currently between `ToolCallComplete` and its
        /// matching `ToolResult` (or the first `ToolCallDelta` carrying
        /// a name, before `ToolCallComplete` arrives). `None` when no
        /// tool is in flight.
        in_flight: Option<ToolUseInFlight>,
    },
    /// The provider call failed on a retryable error and the loop is
    /// waiting out its backoff before replaying the turn — shows the
    /// pending attempt, the wait still to run, the attempt budget, and
    /// the failure's taxonomy class (1 row).
    ///
    /// Entered from the typed `StreamRetry` marker, which the engine
    /// emits BEFORE the wait, and left when the replayed attempt's first
    /// event arrives (back to [`Self::Generating`]) or the turn finishes.
    /// The row is the only place a headless-looking stall becomes
    /// legible, so it stays visible for the whole wait — the render tick
    /// counts [`Self::Retrying::remaining`] down against
    /// [`Self::Retrying::wait_until`].
    Retrying {
        /// 1-based index of the attempt the wait leads to (`2` is the
        /// first retry).
        attempt: u32,
        /// Total attempts the policy allows including the first; `None`
        /// is the unbounded default and renders as the word `unbounded`.
        max_attempts: Option<u32>,
        /// Stable `snake_case` taxonomy class of the failure being
        /// retried, rendered verbatim — never provider free text.
        error_class: String,
        /// Instant the announced wait ends. Held so a render tick can
        /// recompute the countdown without the caller tracking it.
        wait_until: Instant,
        /// Wait still to run as of the last tick — what the row shows.
        remaining: Duration,
        /// Time elapsed since the turn began, refreshed by the same tick.
        /// The turn is still running during the wait, so the input
        /// divider keeps showing its elapsed segment instead of dropping
        /// it for the duration of every retry.
        elapsed: Duration,
    },
    /// Generation has finished — shows the usage summary (1 row).
    Complete {
        /// Pre-formatted usage summary string supplied by NT-011.
        usage_summary: String,
    },
}

impl StreamingIndicator {
    /// Rows this indicator contributes to the fixed panel height.
    ///
    /// Live generation is shown in the input-mode divider, so
    /// [`StreamingIndicator::Generating`] contributes zero body rows.
    /// [`StreamingIndicator::Complete`] contributes one short-lived body
    /// row for the final usage summary, and
    /// [`StreamingIndicator::Retrying`] one row for the duration of the
    /// backoff wait.
    pub const fn height(&self) -> u16 {
        match self {
            Self::Idle | Self::Generating { .. } => 0,
            Self::Retrying { .. } | Self::Complete { .. } => 1,
        }
    }

    /// Coarse key for deciding whether a render tick should repaint.
    ///
    /// This deliberately ignores sub-second elapsed time and token-count
    /// churn so render ticks avoid repainting the controlled panel on
    /// every streamed chunk. The next whole-second or tool/completion
    /// transition will paint the latest token estimate.
    #[must_use]
    pub(crate) fn repaint_key(&self, terminal_cols: u16) -> Option<String> {
        match self {
            Self::Idle => None,
            Self::Generating {
                elapsed, in_flight, ..
            } => {
                let tool = in_flight.as_ref().map_or_else(String::new, |tool| {
                    format!(
                        "{}\n{}",
                        tool.tool_name,
                        tool.description.as_deref().unwrap_or_default()
                    )
                });
                Some(format!(
                    "generating:{}:{terminal_cols}:{tool}",
                    elapsed.as_secs()
                ))
            }
            Self::Retrying {
                attempt,
                max_attempts,
                error_class,
                remaining,
                ..
            } => Some(retry_repaint_key(
                *attempt,
                *max_attempts,
                *remaining,
                error_class,
                terminal_cols,
            )),
            Self::Complete { usage_summary } => Some(usage_summary.clone()),
        }
    }

    /// Render the indicator at zero-based `row`.
    ///
    /// [`StreamingIndicator::Idle`] writes nothing. `Generating` paints
    /// one of three shapes depending on whether a tool is in flight and
    /// whether its description is known:
    /// - no tool in flight: `● generating... {elapsed}s  ~{est}↓`
    /// - tool with description: `● {tool}: '{desc}'  {elapsed}s  ~{est}↓`
    /// - tool without description: `● {tool}  {elapsed}s  ~{est}↓`
    ///
    /// `Retrying` paints the shared retry label
    /// ([`retry_status_label`]) behind the wait glyph, in the same amber
    /// the generating row uses — a wait, not a failure; the failure is
    /// reported loudly on its own path if the retries ever stop.
    ///
    /// `Complete` renders the pre-formatted usage summary verbatim. When
    /// `terminal_cols` does not leave room for the description, the
    /// description is replaced with a Unicode ellipsis so the surrounding
    /// tail (elapsed + token estimate) stays visible.
    pub fn render<W: io::Write>(
        &self,
        row: u16,
        writer: &mut W,
        caps: &TerminalCaps,
        terminal_cols: u16,
    ) -> io::Result<()> {
        match self {
            Self::Idle => Ok(()),
            Self::Generating {
                elapsed,
                est_output_tokens,
                in_flight,
            } => {
                let colour = colour_for(GENERATING_COLOUR, caps);
                let body = format_generating_body(
                    elapsed.as_secs(),
                    *est_output_tokens,
                    in_flight.as_ref(),
                    terminal_cols,
                );
                write!(
                    writer,
                    "{}{}{colour}{body}{}",
                    cursor_to(row),
                    erase_line(),
                    Csi::Sgr(Sgr::Reset),
                )
            }
            Self::Retrying {
                attempt,
                max_attempts,
                error_class,
                remaining,
                ..
            } => {
                let colour = colour_for(GENERATING_COLOUR, caps);
                let body = retry_row_body(
                    *attempt,
                    *max_attempts,
                    *remaining,
                    error_class,
                    terminal_cols,
                );
                write!(
                    writer,
                    "{}{}{colour}{body}{}",
                    cursor_to(row),
                    erase_line(),
                    Csi::Sgr(Sgr::Reset),
                )
            }
            Self::Complete { usage_summary } => {
                write!(writer, "{}{}{usage_summary}", cursor_to(row), erase_line())
            }
        }
    }
}

/// Compose the text body for the `Generating` indicator at the current
/// terminal width.
///
/// `est_output_tokens` is rendered with a `~` prefix and the `↓`
/// direction marker matching the status bar. When `in_flight` carries a
/// description, the description is wrapped in single quotes and
/// truncated with a single-codepoint ellipsis to fit the remaining
/// width budget. When the description is absent (or empty) the line
/// shows only the tool name. When `in_flight` is `None`, the original
/// `generating...` shape is used.
fn format_generating_body(
    secs: u64,
    est_output_tokens: u64,
    in_flight: Option<&ToolUseInFlight>,
    terminal_cols: u16,
) -> String {
    let tail = format!("  {secs}s  ~{}↓", format_count(est_output_tokens));
    let Some(in_flight) = in_flight else {
        return format!("● generating...{tail}");
    };
    let head = format!("● {}", in_flight.tool_name);
    let description = in_flight
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty());
    let Some(description) = description else {
        return format!("{head}{tail}");
    };
    // Budget for the description segment = total - head - quoted wrap
    // (": ''" = 4 cols) - tail. Caller-side truncation keeps the tail
    // (elapsed + token estimate) visible so the user can still see
    // progress even when descriptions are long.
    let head_cols = u16::try_from(head.width()).unwrap_or(u16::MAX);
    let tail_cols = u16::try_from(tail.width()).unwrap_or(u16::MAX);
    let wrap_cols: u16 = 4; // ": '" + "'"
    let budget = terminal_cols
        .saturating_sub(head_cols)
        .saturating_sub(tail_cols)
        .saturating_sub(wrap_cols);
    let trimmed = truncate_with_ellipsis(description, budget);
    if trimmed.is_empty() {
        // No room for any description text — collapse to the no-desc
        // form rather than rendering empty quotes.
        return format!("{head}{tail}");
    }
    format!("{head}: '{trimmed}'{tail}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::render::retry_status::retry_row_body;

    /// A retry wait for a bounded policy: the row names the pending
    /// attempt, the countdown, the budget, and the taxonomy class.
    #[test]
    fn retrying_indicator_renders_the_wait_attempt_budget_and_class() {
        let indicator = StreamingIndicator::Retrying {
            attempt: 3,
            max_attempts: Some(5),
            error_class: "server_error".to_string(),
            wait_until: Instant::now() + Duration::from_secs(8),
            remaining: Duration::from_secs(8),
            elapsed: Duration::from_secs(12),
        };
        assert_eq!(
            indicator.height(),
            1,
            "the wait must occupy a visible row for its whole duration"
        );

        let mut buf: Vec<u8> = Vec::new();
        indicator
            .render(0, &mut buf, &TerminalCaps::baseline(), 120)
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("retrying in 8s (attempt 3 of 5, server_error)"),
            "the retry row must state the whole wait: {out:?}"
        );
        assert!(
            out.contains(&retry_row_body(
                3,
                Some(5),
                Duration::from_secs(8),
                "server_error",
                120
            )),
            "the row is the shared retry body, glyph included: {out:?}"
        );
    }

    /// The default policy is unbounded; the row says so in words.
    #[test]
    fn retrying_indicator_spells_out_an_unbounded_budget() {
        let indicator = StreamingIndicator::Retrying {
            attempt: 7,
            max_attempts: None,
            error_class: "rate_limited".to_string(),
            wait_until: Instant::now() + Duration::from_secs(30),
            remaining: Duration::from_secs(30),
            elapsed: Duration::from_secs(90),
        };
        let mut buf: Vec<u8> = Vec::new();
        indicator
            .render(0, &mut buf, &TerminalCaps::baseline(), 120)
            .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("retrying in 30s (attempt 7, unbounded, rate_limited)"),
            "got: {out:?}"
        );
    }

    /// The repaint key moves with the countdown's whole seconds — one
    /// repaint per second, not one per tick — and is stable within a
    /// second.
    #[test]
    fn retrying_repaint_key_follows_whole_seconds_only() {
        let wait_until = Instant::now() + Duration::from_secs(8);
        let key_for = |remaining: Duration| {
            StreamingIndicator::Retrying {
                attempt: 2,
                max_attempts: None,
                error_class: "timeout".to_string(),
                wait_until,
                remaining,
                elapsed: Duration::from_secs(1),
            }
            .repaint_key(80)
        };
        assert_eq!(
            key_for(Duration::from_millis(7_100)),
            key_for(Duration::from_millis(7_999)),
            "sub-second countdown movement must not repaint"
        );
        assert_ne!(
            key_for(Duration::from_millis(7_000)),
            key_for(Duration::from_millis(6_000)),
            "each whole second of the countdown repaints"
        );
    }

    #[test]
    fn streaming_indicator_height_tracks_state() {
        assert_eq!(StreamingIndicator::Idle.height(), 0);
        assert_eq!(
            StreamingIndicator::Generating {
                elapsed: Duration::from_secs(3),
                est_output_tokens: 0,
                in_flight: None,
            }
            .height(),
            0
        );
        assert_eq!(
            StreamingIndicator::Complete {
                usage_summary: "[1 in / 1 out, 0.1s]".to_string(),
            }
            .height(),
            1
        );
    }

    #[test]
    fn streaming_indicator_generating_renders_elapsed_time() {
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        StreamingIndicator::Generating {
            elapsed: Duration::from_secs(5),
            est_output_tokens: 0,
            in_flight: None,
        }
        .render(0, &mut buf, &caps, 80)
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("generating"));
        assert!(
            out.contains("5s"),
            "elapsed seconds must appear, got: {out:?}"
        );
    }

    #[test]
    fn streaming_indicator_idle_renders_nothing() {
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        StreamingIndicator::Idle
            .render(0, &mut buf, &caps, 80)
            .unwrap();
        assert!(buf.is_empty());
    }

    #[test]
    fn streaming_indicator_generating_shows_token_estimate_with_tilde() {
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        StreamingIndicator::Generating {
            elapsed: Duration::from_secs(2),
            est_output_tokens: 1_234,
            in_flight: None,
        }
        .render(0, &mut buf, &caps, 80)
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("~1,234↓"),
            "tilde-prefixed estimate with ↓ marker: {out:?}"
        );
    }

    #[test]
    fn streaming_indicator_generating_with_tool_and_description_shows_quotes() {
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        StreamingIndicator::Generating {
            elapsed: Duration::from_secs(2),
            est_output_tokens: 500,
            in_flight: Some(ToolUseInFlight {
                tool_name: "bash".to_string(),
                description: Some("listing docs folder".to_string()),
            }),
        }
        .render(0, &mut buf, &caps, 80)
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("● bash:"), "tool name with colon: {out:?}");
        assert!(
            out.contains("'listing docs folder'"),
            "description wrapped in single quotes: {out:?}"
        );
        assert!(
            !out.contains("generating..."),
            "generating... must not appear when tool is in flight: {out:?}"
        );
    }

    #[test]
    fn streaming_indicator_generating_with_tool_no_description_omits_quotes() {
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        StreamingIndicator::Generating {
            elapsed: Duration::from_secs(1),
            est_output_tokens: 100,
            in_flight: Some(ToolUseInFlight {
                tool_name: "read".to_string(),
                description: None,
            }),
        }
        .render(0, &mut buf, &caps, 80)
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("● read"), "tool name appears: {out:?}");
        assert!(
            !out.contains("''"),
            "no empty quotes when description is None: {out:?}"
        );
        assert!(
            !out.contains(": '"),
            "no colon-quote when description is None: {out:?}"
        );
    }

    #[test]
    fn streaming_indicator_generating_with_empty_description_omits_quotes() {
        // Some("") from split_envelope_fields when the model populates
        // the envelope key with a blank string — treat as missing.
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        StreamingIndicator::Generating {
            elapsed: Duration::from_secs(1),
            est_output_tokens: 100,
            in_flight: Some(ToolUseInFlight {
                tool_name: "read".to_string(),
                description: Some(String::new()),
            }),
        }
        .render(0, &mut buf, &caps, 80)
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            !out.contains("''"),
            "empty description must not render empty quotes: {out:?}"
        );
    }

    #[test]
    fn streaming_indicator_generating_truncates_long_description_with_ellipsis() {
        // Narrow terminal (40 cols) forces truncation; the description
        // must lose tail characters to a Unicode ellipsis but the tail
        // (elapsed + token estimate) must stay visible.
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        let long = "this description is far too long to fit on a forty-column row";
        StreamingIndicator::Generating {
            elapsed: Duration::from_secs(2),
            est_output_tokens: 100,
            in_flight: Some(ToolUseInFlight {
                tool_name: "bash".to_string(),
                description: Some(long.to_string()),
            }),
        }
        .render(0, &mut buf, &caps, 40)
        .unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains('\u{2026}'), "ellipsis: {out:?}");
        assert!(out.contains("2s"), "elapsed survives: {out:?}");
        assert!(out.contains("~100↓"), "token estimate survives: {out:?}");
    }
}
