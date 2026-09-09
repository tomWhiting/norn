//! Terminal capability detection.
//!
//! Colour depth comes from declared environment capabilities and never gates
//! startup. Keyboard, synchronized rendering, hyperlinks and italic are
//! progressive enhancements probed after entering raw mode.

use std::env;
use std::io::{self, Write as _};
use std::time::Duration;

use termina::escape::csi::{self, Csi, DecModeSetting, DecPrivateMode, DecPrivateModeCode};
use termina::{Event, PlatformTerminal, Terminal};

use super::colour::ColourDepth;

/// Detected terminal capabilities.
///
/// Colour uses explicit degradation; optional enhancements use terminal queries.
#[derive(Clone, Debug)]
pub struct TerminalCaps {
    /// Richest colour encoding supported by the startup evidence.
    pub colour_depth: ColourDepth,
    /// Terminal supports the Kitty keyboard protocol.
    pub kitty_keyboard: bool,
    /// Terminal supports DCS 2026 synchronized rendering.
    pub synchronized_rendering: bool,
    /// Terminal supports OSC 8 hyperlinks.
    pub osc_hyperlinks: bool,
    /// Terminal supports the italic SGR attribute.
    pub italic_support: bool,
}

impl TerminalCaps {
    /// Probe terminal capabilities by writing query sequences and reading
    /// responses. Must be called after entering raw mode.
    pub fn detect(terminal: &mut PlatformTerminal) -> io::Result<Self> {
        let mut caps = Self {
            colour_depth: ColourDepth::from_environment(
                env::var("TERM").ok().as_deref(),
                env::var("COLORTERM").ok().as_deref(),
            ),
            kitty_keyboard: false,
            synchronized_rendering: false,
            osc_hyperlinks: Self::env_has_osc8(),
            italic_support: false,
        };

        write!(
            terminal,
            "{}{}{}",
            Csi::Keyboard(csi::Keyboard::QueryFlags),
            Csi::Mode(csi::Mode::QueryDecPrivateMode(DecPrivateMode::Code(
                DecPrivateModeCode::SynchronizedOutput,
            ))),
            Csi::Device(csi::Device::RequestPrimaryDeviceAttributes),
        )?;
        terminal.flush()?;

        let mut timeout = Some(Duration::from_millis(150));
        while terminal.poll(Event::is_escape, timeout)? {
            match terminal.read(Event::is_escape)? {
                Event::Csi(Csi::Keyboard(csi::Keyboard::ReportFlags(_))) => {
                    caps.kitty_keyboard = true;
                }
                Event::Csi(Csi::Mode(csi::Mode::ReportDecPrivateMode {
                    mode: DecPrivateMode::Code(DecPrivateModeCode::SynchronizedOutput),
                    setting,
                })) => {
                    caps.synchronized_rendering = setting != DecModeSetting::NotRecognized;
                }
                Event::Csi(Csi::Device(csi::Device::DeviceAttributes(()))) => {
                    caps.italic_support = true;
                    // Primary DA is not proof that earlier enhancement replies were received.
                    // Drain replies already queued without extending terminal admission.
                    timeout = Some(Duration::ZERO);
                }
                _ => {}
            }
        }

        Ok(caps)
    }

    /// Construct the original indexed-colour baseline without optional enhancements.
    pub fn baseline() -> Self {
        Self {
            colour_depth: ColourDepth::Indexed256,
            kitty_keyboard: false,
            synchronized_rendering: false,
            osc_hyperlinks: false,
            italic_support: false,
        }
    }

    fn env_has_osc8() -> bool {
        if let Ok(program) = env::var("TERM_PROGRAM") {
            return matches!(
                program.as_str(),
                "iTerm.app" | "WezTerm" | "ghostty" | "Rio"
            );
        }
        env::var("KITTY_PID").is_ok()
    }
}

impl Default for TerminalCaps {
    fn default() -> Self {
        Self::baseline()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_has_no_enhancements() {
        let caps = TerminalCaps::baseline();
        assert_eq!(caps.colour_depth, ColourDepth::Indexed256);
        assert!(!caps.kitty_keyboard);
        assert!(!caps.synchronized_rendering);
        assert!(!caps.osc_hyperlinks);
        assert!(!caps.italic_support);
    }

    #[test]
    fn default_matches_baseline() {
        let caps = TerminalCaps::default();
        assert_eq!(caps.colour_depth, ColourDepth::Indexed256);
        assert!(!caps.kitty_keyboard);
    }
}
