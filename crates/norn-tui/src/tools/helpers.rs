//! Shared styling and formatting helpers for tool renderers.
//!
//! Extracted from [`super::rich`] so each renderer module stays under the
//! 500-line production-code cap. All helpers are pure functions — they
//! depend only on their arguments and [`TerminalCaps`].

use std::fmt::Write as _;

use serde_json::Value;
use termina::escape::csi::{Csi, Sgr};
use termina::style::{ColorSpec, Intensity, RgbColor};

use crate::render::colour_for;
use crate::terminal::caps::TerminalCaps;

/// Maximum byte length of a command/query preview embedded in a header.
pub const HEADER_PREVIEW_MAX_BYTES: usize = 120;

/// Error-state foreground colour (non-zero exit, AST blocked, I/O error).
pub const RED: RgbColor = RgbColor::new(200, 80, 80);

/// Added-line foreground colour for unified diffs.
pub const GREEN: RgbColor = RgbColor::new(80, 180, 80);

/// Warning/override foreground colour — matches the streaming
/// indicator's amber in `render::fixed_panel`.
pub const AMBER: RgbColor = RgbColor::new(215, 175, 0);

/// Spinner glyph for in-progress streaming headers.
///
/// U+27F3 (CLOCKWISE GAPPED CIRCLE ARROW) reads as "working" without
/// implying a definite progress fraction.
pub const SPINNER: char = '⟳';

/// Foreground-colour SGR escape for `rgb`, adapted to `caps`.
pub fn fg(rgb: RgbColor, caps: &TerminalCaps) -> String {
    colour_for(rgb, caps)
}

/// SGR escape that resets the foreground colour to the terminal default.
pub fn fg_reset() -> String {
    Csi::Sgr(Sgr::Foreground(ColorSpec::Reset)).to_string()
}

/// SGR escape enabling the dim attribute.
pub fn dim() -> String {
    Csi::Sgr(Sgr::Intensity(Intensity::Dim)).to_string()
}

/// SGR escape enabling the bold attribute.
pub fn bold() -> String {
    Csi::Sgr(Sgr::Intensity(Intensity::Bold)).to_string()
}

/// Full SGR reset — clears colour and attributes.
pub fn reset() -> String {
    Csi::Sgr(Sgr::Reset).to_string()
}

/// Formats a millisecond duration as `0.42s` (`< 60s`) or `1m 23s`.
pub fn format_duration_ms(ms: u64) -> String {
    if ms < 60_000 {
        format!("{}.{:02}s", ms / 1000, (ms % 1000) / 10)
    } else {
        let secs = ms / 1000;
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// Truncates `s` to at most [`HEADER_PREVIEW_MAX_BYTES`] on a UTF-8 char
/// boundary, appending `…` when truncation occurred.
pub fn truncate_preview(s: &str) -> String {
    if s.len() <= HEADER_PREVIEW_MAX_BYTES {
        return s.to_string();
    }
    let end = s
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= HEADER_PREVIEW_MAX_BYTES)
        .last()
        .unwrap_or(0);
    format!("{}…", &s[..end])
}

/// Colourises a unified-diff string: `+` lines green, `-` lines red,
/// `@@`/`---`/`+++` headers dim, context lines unstyled.
///
/// Shared by [`super::rich::EditRenderer`] and
/// [`super::rich::ApplyPatchRenderer`] so their diff bodies render
/// identically.
pub fn colourise_unified_diff(diff: &str, caps: &TerminalCaps) -> String {
    let mut out = String::with_capacity(diff.len());
    for line in diff.lines() {
        if line.starts_with("@@") || line.starts_with("---") || line.starts_with("+++") {
            let _ = write!(out, "{}{line}{}", dim(), reset());
        } else if line.starts_with('+') {
            let _ = write!(out, "{}{line}{}", fg(GREEN, caps), fg_reset());
        } else if line.starts_with('-') {
            let _ = write!(out, "{}{line}{}", fg(RED, caps), fg_reset());
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Renders the `diagnostics` array of a tool result as indented lines.
///
/// Each entry renders as `  line {line}:{column} [{severity}] {code}:
/// {message}`. With `severity_colours`, `error` entries render red and
/// `warning` entries amber; otherwise entries are unstyled.
pub fn render_diagnostics(result: &Value, caps: &TerminalCaps, severity_colours: bool) -> String {
    let Some(diags) = result.get("diagnostics").and_then(Value::as_array) else {
        return String::new();
    };
    let mut out = String::new();
    for d in diags {
        let line = d.get("line").and_then(Value::as_u64).unwrap_or(0);
        let column = d.get("column").and_then(Value::as_u64).unwrap_or(0);
        let severity = d.get("severity").and_then(Value::as_str).unwrap_or("");
        let code = d.get("code").and_then(Value::as_str).unwrap_or("");
        let message = d.get("message").and_then(Value::as_str).unwrap_or("");
        let text = format!("  line {line}:{column} [{severity}] {code}: {message}");
        let colour = if severity_colours {
            match severity {
                "error" => Some(RED),
                "warning" => Some(AMBER),
                _ => None,
            }
        } else {
            None
        };
        match colour {
            Some(c) => {
                let _ = writeln!(out, "{}{text}{}", fg(c, caps), fg_reset());
            }
            None => {
                let _ = writeln!(out, "{text}");
            }
        }
    }
    out
}

/// True when `result.check_overrides` is a non-empty array.
pub fn has_overrides(result: &Value) -> bool {
    result
        .get("check_overrides")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
}

/// Source attribution of the first `check_overrides` entry, or
/// `(unknown)` when absent or empty.
pub fn override_source(result: &Value) -> String {
    let source = result
        .get("check_overrides")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|o| o.get("source"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if source.is_empty() {
        "(unknown)".to_string()
    } else {
        source.to_string()
    }
}

/// Extracts a string field from `args`, falling back to `result`.
pub fn string_field(args: &Value, result: &Value, key: &str) -> String {
    args.get(key)
        .and_then(Value::as_str)
        .or_else(|| result.get(key).and_then(Value::as_str))
        .unwrap_or("")
        .to_string()
}

/// Best-effort extraction of a string field from a partial-JSON
/// streaming-argument fragment.
pub fn partial_field(partial_args: &str, key: &str) -> Option<String> {
    serde_json::from_str::<Value>(partial_args)
        .ok()
        .and_then(|v| {
            v.get(key)
                .and_then(Value::as_str)
                .map(std::string::ToString::to_string)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_formats_sub_minute_and_over_minute() {
        assert_eq!(format_duration_ms(420), "0.42s");
        assert_eq!(format_duration_ms(1_000), "1.00s");
        assert_eq!(format_duration_ms(83_000), "1m 23s");
    }

    #[test]
    fn truncate_preview_respects_char_boundary() {
        let long = "é".repeat(200);
        let truncated = truncate_preview(&long);
        assert!(truncated.ends_with('…'));
        assert!(truncated.len() <= HEADER_PREVIEW_MAX_BYTES + '…'.len_utf8() + 1);
    }
}
