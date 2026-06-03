//! `TerminalCaps`-aware styling.
//!
//! Every styling decision routes through this module so that rendering
//! code stays capability-agnostic. Each helper consults [`TerminalCaps`]
//! and emits the richest form the terminal supports, falling back
//! gracefully otherwise:
//!
//! - [`colour_for`] — 24-bit RGB, or the nearest 256-colour palette entry.
//! - [`italic`] — the italic SGR, or underline.
//! - [`newline_key_hint`] — the Kitty-protocol newline key, or `Alt+Enter`.
//! - [`hyperlink`] — an OSC 8 hyperlink, or `text (url)` bracketed text.
//! - [`sync_render`] — DEC mode 2026 synchronized output, or cursor
//!   hide/show.

use std::io;

use termina::escape::csi::{Csi, DecPrivateMode, DecPrivateModeCode, Mode, Sgr};
use termina::escape::{OSC, ST};
use termina::style::{ColorSpec, RgbColor, Underline};

use crate::terminal::caps::TerminalCaps;

/// The xterm 256-colour cube channel levels (indices 16-231).
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// Standard xterm RGB values for the 16 base ANSI colours (indices 0-15).
const BASE_ANSI: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (128, 0, 0),
    (0, 128, 0),
    (128, 128, 0),
    (0, 0, 128),
    (128, 0, 128),
    (0, 128, 128),
    (192, 192, 192),
    (128, 128, 128),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (0, 0, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

/// Resolve the RGB triple for a 256-colour palette index.
fn palette_rgb(index: u8) -> (u8, u8, u8) {
    match index {
        0..=15 => BASE_ANSI[usize::from(index)],
        16..=231 => {
            let i = index - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            (
                CUBE_LEVELS[usize::from(r)],
                CUBE_LEVELS[usize::from(g)],
                CUBE_LEVELS[usize::from(b)],
            )
        }
        232..=255 => {
            let level = 8 + (index - 232) * 10;
            (level, level, level)
        }
    }
}

/// Find the 256-colour palette index nearest to `rgb`.
///
/// Searches the full standard 256-colour palette — the 16 base ANSI
/// colours, the 6x6x6 colour cube, and the 24-step grayscale ramp — and
/// returns the index minimising the Euclidean distance in RGB space.
/// On ties the lowest index wins.
pub fn nearest_256(rgb: RgbColor) -> u8 {
    let mut best_index = 0u8;
    let mut best_dist = u32::MAX;
    for index in 0..=u8::MAX {
        let (r, g, b) = palette_rgb(index);
        let dr = u32::from(rgb.red).abs_diff(u32::from(r));
        let dg = u32::from(rgb.green).abs_diff(u32::from(g));
        let db = u32::from(rgb.blue).abs_diff(u32::from(b));
        let dist = dr * dr + dg * dg + db * db;
        if dist < best_dist {
            best_dist = dist;
            best_index = index;
        }
    }
    best_index
}

/// Map an RGB colour to a [`ColorSpec`] honouring terminal capabilities.
///
/// Returns a true-colour spec when [`TerminalCaps::true_colour`] is set,
/// otherwise the nearest 256-colour palette entry (see [`nearest_256`]).
pub fn colour_spec(rgb: RgbColor, caps: &TerminalCaps) -> ColorSpec {
    if caps.true_colour {
        ColorSpec::TrueColor(rgb.into())
    } else {
        ColorSpec::PaletteIndex(nearest_256(rgb))
    }
}

/// Build the foreground SGR escape sequence for an RGB colour.
///
/// Emits a 24-bit `38;2;r;g;b` escape when the terminal supports true
/// colour, otherwise a `38;5;{index}` escape targeting the nearest
/// 256-colour palette entry.
pub fn colour_for(rgb: RgbColor, caps: &TerminalCaps) -> String {
    Csi::Sgr(Sgr::Foreground(colour_spec(rgb, caps))).to_string()
}

/// The SGR attribute to use for emphasised (italic) text.
///
/// Returns the italic SGR when the terminal advertises italic support,
/// otherwise a single underline as a graceful fallback.
pub fn italic(caps: &TerminalCaps) -> Sgr {
    if caps.italic_support {
        Sgr::Italic(true)
    } else {
        Sgr::Underline(Underline::Single)
    }
}

/// The key-combination hint for inserting a newline in the input area.
///
/// Returns `Shift+Enter` when the Kitty keyboard protocol is available
/// (it can reliably discriminate `Shift+Enter`), otherwise `Alt+Enter`.
pub fn newline_key_hint(caps: &TerminalCaps) -> &'static str {
    if caps.kitty_keyboard {
        "Shift+Enter"
    } else {
        "Alt+Enter"
    }
}

/// Render `text` as a hyperlink to `url`.
///
/// Emits an OSC 8 hyperlink escape (`ESC ] 8 ; ; url ST text ESC ] 8 ; ;
/// ST`) when the terminal supports OSC 8, otherwise the bracketed
/// `text (url)` plain-text form.
pub fn hyperlink(text: &str, url: &str, caps: &TerminalCaps) -> String {
    if caps.osc_hyperlinks {
        format!("{OSC}8;;{url}{ST}{text}{OSC}8;;{ST}")
    } else {
        format!("{text} ({url})")
    }
}

/// Compute the `(prefix, suffix)` escape pair that brackets a redraw.
///
/// Exposed `pub(crate)` so [`crate::app::helpers::sync_with_guard`] can
/// reuse the same prefix/suffix pair around a `&mut TerminalGuard`
/// closure — `sync_render`'s `&mut W` shape locks the writer for the
/// duration and so cannot host helpers that also need the guard.
pub(crate) fn sync_markers(caps: &TerminalCaps) -> (Csi, Csi) {
    if caps.synchronized_rendering {
        let mode = DecPrivateMode::Code(DecPrivateModeCode::SynchronizedOutput);
        (
            Csi::Mode(Mode::SetDecPrivateMode(mode)),
            Csi::Mode(Mode::ResetDecPrivateMode(mode)),
        )
    } else {
        let cursor = DecPrivateMode::Code(DecPrivateModeCode::ShowCursor);
        (
            Csi::Mode(Mode::ResetDecPrivateMode(cursor)),
            Csi::Mode(Mode::SetDecPrivateMode(cursor)),
        )
    }
}

/// Run `body` wrapped in synchronized-rendering markers.
///
/// When [`TerminalCaps::synchronized_rendering`] is set, `body` runs
/// between DEC private mode 2026 set/reset escapes so the terminal
/// presents the redraw atomically. Otherwise the redraw is bracketed by
/// cursor hide/show escapes to avoid a visible cursor flicker.
///
/// The closing escape is always emitted regardless of whether the
/// prefix write succeeded or the body returned an error, so the
/// terminal is never left in synchronized-output or hidden-cursor
/// state. The body is only invoked when the prefix write succeeds —
/// running body content without its sync bracket would defeat the
/// flicker guarantee. Error precedence: prefix > body > suffix.
pub fn sync_render<W, F>(caps: &TerminalCaps, writer: &mut W, body: F) -> io::Result<()>
where
    W: io::Write,
    F: FnOnce(&mut W) -> io::Result<()>,
{
    let (prefix, suffix) = sync_markers(caps);
    let prefix_attempt = write!(writer, "{prefix}");
    let result = prefix_attempt.and_then(|()| body(writer));
    let suffix_attempt = write!(writer, "{suffix}");
    result.and(suffix_attempt)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn caps_with(mutate: impl FnOnce(&mut TerminalCaps)) -> TerminalCaps {
        let mut caps = TerminalCaps::baseline();
        mutate(&mut caps);
        caps
    }

    #[test]
    fn colour_for_emits_rgb_escape_with_true_colour() {
        let caps = caps_with(|c| c.true_colour = true);
        let escape = colour_for(RgbColor::new(10, 20, 30), &caps);
        assert!(escape.contains("38;2;10;20;30"), "got: {escape:?}");
    }

    #[test]
    fn nearest_256_maps_exact_cube_point() {
        // RGB(95, 135, 175) is cube point r=1, g=2, b=3 → 16 + 36 + 12 + 3.
        assert_eq!(nearest_256(RgbColor::new(95, 135, 175)), 67);
    }

    #[test]
    fn colour_for_falls_back_to_palette_index() {
        let caps = TerminalCaps::baseline();
        let escape = colour_for(RgbColor::new(95, 135, 175), &caps);
        assert!(escape.contains("38;5;67"), "got: {escape:?}");
    }

    #[test]
    fn colour_spec_variants_track_capability() {
        let true_caps = caps_with(|c| c.true_colour = true);
        assert!(matches!(
            colour_spec(RgbColor::new(1, 2, 3), &true_caps),
            ColorSpec::TrueColor(_)
        ));
        let baseline = TerminalCaps::baseline();
        assert!(matches!(
            colour_spec(RgbColor::new(95, 135, 175), &baseline),
            ColorSpec::PaletteIndex(67)
        ));
    }

    #[test]
    fn italic_falls_back_to_underline() {
        let baseline = TerminalCaps::baseline();
        assert_eq!(italic(&baseline), Sgr::Underline(Underline::Single));
        let italic_caps = caps_with(|c| c.italic_support = true);
        assert_eq!(italic(&italic_caps), Sgr::Italic(true));
    }

    #[test]
    fn newline_key_hint_tracks_kitty_support() {
        assert_eq!(newline_key_hint(&TerminalCaps::baseline()), "Alt+Enter");
        let kitty = caps_with(|c| c.kitty_keyboard = true);
        assert_eq!(newline_key_hint(&kitty), "Shift+Enter");
    }

    #[test]
    fn hyperlink_falls_back_to_bracketed_text() {
        let baseline = TerminalCaps::baseline();
        assert_eq!(
            hyperlink("README", "file:///README.md", &baseline),
            "README (file:///README.md)"
        );
    }

    #[test]
    fn hyperlink_emits_osc8_when_supported() {
        let caps = caps_with(|c| c.osc_hyperlinks = true);
        let link = hyperlink("README", "file:///README.md", &caps);
        assert!(link.contains("\x1b]8;;file:///README.md"), "got: {link:?}");
        assert!(link.contains("README"));
        assert!(link.ends_with("\x1b]8;;\x1b\\"));
    }

    #[test]
    fn sync_render_uses_cursor_hide_show_without_synchronized() {
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        sync_render(&caps, &mut buf, |w| write!(w, "BODY")).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(
            out.contains("\x1b[?25l"),
            "expected cursor hide, got: {out:?}"
        );
        assert!(
            out.contains("\x1b[?25h"),
            "expected cursor show, got: {out:?}"
        );
        assert!(out.contains("BODY"));
        assert!(
            out.find("\x1b[?25l") < out.find("BODY"),
            "hide must precede body"
        );
        assert!(
            out.find("BODY") < out.find("\x1b[?25h"),
            "show must follow body"
        );
    }

    #[test]
    fn sync_render_uses_dec_2026_when_synchronized() {
        let caps = caps_with(|c| c.synchronized_rendering = true);
        let mut buf: Vec<u8> = Vec::new();
        sync_render(&caps, &mut buf, |w| write!(w, "BODY")).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("\x1b[?2026h"), "got: {out:?}");
        assert!(out.contains("\x1b[?2026l"), "got: {out:?}");
    }

    #[test]
    fn sync_render_emits_suffix_even_when_body_fails() {
        let caps = TerminalCaps::baseline();
        let mut buf: Vec<u8> = Vec::new();
        let result = sync_render(&caps, &mut buf, |_w| Err(io::Error::other("boom")));
        assert!(result.is_err());
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("\x1b[?25h"), "suffix must still be emitted");
    }

    #[test]
    fn sync_render_emits_suffix_even_when_prefix_fails() {
        // A partial-write failure on the prefix could leave the
        // terminal stuck (cursor hidden, or in synchronized-output
        // state) if the suffix is then skipped. The contract is
        // "always restore terminal state" — the suffix must be
        // attempted regardless. Body is skipped when prefix fails
        // because writing body content without its sync bracket
        // defeats the flicker guarantee.
        struct FailFirstWrite {
            first: bool,
            buf: Vec<u8>,
        }
        impl io::Write for FailFirstWrite {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if self.first {
                    self.first = false;
                    return Err(io::Error::other("prefix boom"));
                }
                self.buf.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let caps = TerminalCaps::baseline();
        let mut writer = FailFirstWrite {
            first: true,
            buf: Vec::new(),
        };
        let result = sync_render(&caps, &mut writer, |w| write!(w, "BODY"));
        assert!(result.is_err(), "prefix error must propagate");
        let out = String::from_utf8(writer.buf).unwrap();
        assert!(
            out.contains("\x1b[?25h"),
            "suffix must still be attempted after prefix failure: {out:?}",
        );
        assert!(
            !out.contains("BODY"),
            "body must be skipped when prefix fails: {out:?}",
        );
    }
}
