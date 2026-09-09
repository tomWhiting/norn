//! Colour evidence and explicit degradation; no startup refusal or extra I/O.

/// One consistent colour policy for transcript, chrome and Iridium cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColourDepth {
    /// Use the terminal's configured foreground and background.
    Monochrome,
    /// Use only the sixteen basic ANSI colours.
    Ansi16,
    /// Use the xterm 256-entry palette.
    Indexed256,
    /// Use direct 24-bit RGB colour.
    TrueColour,
}

impl ColourDepth {
    /// Classify explicit environment evidence without requiring terminfo.
    ///
    /// TC-001 records the owner ruling for the modern terminal names. Unknown
    /// names use basic ANSI; absent RGB evidence and missing or dumb terminal
    /// names use default colours.
    #[must_use]
    pub fn from_environment(term: Option<&str>, colorterm: Option<&str>) -> Self {
        if matches!(colorterm, Some("truecolor" | "24bit")) {
            return Self::TrueColour;
        }
        let Some(term) = term.filter(|name| !name.is_empty()) else {
            return Self::Monochrome;
        };
        if term == "dumb" {
            return Self::Monochrome;
        }
        if term.contains("256color")
            || ["ghostty", "kitty", "alacritty", "wezterm"]
                .iter()
                .any(|suffix| term.ends_with(suffix))
        {
            return Self::Indexed256;
        }
        Self::Ansi16
    }

    /// One startup notice for degraded colour, retained inside the interface.
    #[must_use]
    pub const fn notice(self) -> Option<&'static str> {
        match self {
            Self::Monochrome => {
                Some("Terminal colour is unconfirmed; using default foreground and background.")
            }
            Self::Ansi16 => Some("Extended terminal colour is unconfirmed; using 16 ANSI colours."),
            Self::Indexed256 | Self::TrueColour => None,
        }
    }
}

#[cfg(test)]
#[path = "colour_tests.rs"]
mod tests;
