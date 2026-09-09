//! Environment-only colour classification and degraded-startup notices.

use super::ColourDepth;

#[test]
fn modern_terminal_names_need_neither_colorterm_nor_terminfo() {
    for term in [
        "xterm-ghostty",
        "xterm-kitty",
        "alacritty",
        "wezterm",
        "xterm-256color",
        "screen-256color",
        "tmux-256color",
    ] {
        assert_eq!(
            ColourDepth::from_environment(Some(term), None),
            ColourDepth::Indexed256,
            "{term}"
        );
    }
}

#[test]
fn explicit_rgb_evidence_wins_over_missing_or_limited_term() {
    for term in [
        None,
        Some(""),
        Some("dumb"),
        Some("xterm"),
        Some("xterm-ghostty"),
    ] {
        for colorterm in ["truecolor", "24bit"] {
            assert_eq!(
                ColourDepth::from_environment(term, Some(colorterm)),
                ColourDepth::TrueColour
            );
        }
    }
}

#[test]
fn missing_or_dumb_term_without_rgb_evidence_keeps_default_colours() {
    for term in [None, Some(""), Some("dumb")] {
        for colorterm in [None, Some(""), Some("unknown")] {
            assert_eq!(
                ColourDepth::from_environment(term, colorterm),
                ColourDepth::Monochrome
            );
        }
    }
}

#[test]
fn unknown_extended_colour_evidence_uses_basic_ansi() {
    for term in [
        "xterm",
        "vt100",
        "screen",
        "tmux",
        "linux",
        "unknown-terminal",
    ] {
        for colorterm in [None, Some(""), Some("yes"), Some("unknown")] {
            assert_eq!(
                ColourDepth::from_environment(Some(term), colorterm),
                ColourDepth::Ansi16
            );
        }
    }
}

#[test]
fn only_degraded_depths_need_a_startup_notice() {
    assert!(ColourDepth::Monochrome.notice().is_some());
    assert!(ColourDepth::Ansi16.notice().is_some());
    assert_eq!(ColourDepth::Indexed256.notice(), None);
    assert_eq!(ColourDepth::TrueColour.notice(), None);
}
