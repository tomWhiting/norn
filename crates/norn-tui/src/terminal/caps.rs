//! Terminal capability detection.
//!
//! Hard requirements (256-colour) are checked from environment variables
//! before entering raw mode. Enhancement capabilities (true colour, Kitty
//! keyboard, synchronized rendering, OSC 8, italic) are probed after
//! entering raw mode by writing query sequences and reading responses.

use std::env;
use std::io::{self, Write as _};
use std::time::Duration;

use termina::escape::csi::{self, Csi, DecModeSetting, DecPrivateMode, DecPrivateModeCode};
use termina::{Event, PlatformTerminal, Terminal};

use crate::TuiError;

/// Detected terminal capabilities.
///
/// Capabilities are split into hard requirements (checked via env vars)
/// and progressive enhancements (probed via terminal queries).
#[derive(Clone, Debug)]
pub struct TerminalCaps {
    /// Terminal supports 24-bit RGB colour.
    pub true_colour: bool,
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
    /// Check hard requirements using environment variables only.
    ///
    /// This runs before raw mode and terminal setup. Returns
    /// `Err(TuiError::UnsupportedTerminal)` if the terminal cannot
    /// support the minimum 256-colour mode.
    pub fn check_hard_requirements() -> Result<(), TuiError> {
        if !Self::env_has_256_colour() {
            return Err(TuiError::UnsupportedTerminal(
                "terminal does not support 256-colour mode. \
                 Set TERM to a 256color variant (e.g. xterm-256color) \
                 or set COLORTERM. Use --print for headless mode."
                    .into(),
            ));
        }
        Ok(())
    }

    /// Probe terminal capabilities by writing query sequences and reading
    /// responses. Must be called after entering raw mode.
    pub fn detect(terminal: &mut PlatformTerminal) -> io::Result<Self> {
        let mut caps = Self {
            true_colour: Self::env_has_true_colour(),
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

        let timeout = Some(Duration::from_millis(150));
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
                    break;
                }
                _ => {}
            }
        }

        Ok(caps)
    }

    /// Construct with all enhancements disabled. For testing only.
    pub fn baseline() -> Self {
        Self {
            true_colour: false,
            kitty_keyboard: false,
            synchronized_rendering: false,
            osc_hyperlinks: false,
            italic_support: false,
        }
    }

    fn env_has_256_colour() -> bool {
        if env::var("COLORTERM").is_ok() {
            return true;
        }
        env::var("TERM").is_ok_and(|t| t.contains("256color"))
    }

    fn env_has_true_colour() -> bool {
        matches!(env::var("COLORTERM").as_deref(), Ok("truecolor" | "24bit"))
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
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn baseline_has_no_enhancements() {
        let caps = TerminalCaps::baseline();
        assert!(!caps.true_colour);
        assert!(!caps.kitty_keyboard);
        assert!(!caps.synchronized_rendering);
        assert!(!caps.osc_hyperlinks);
        assert!(!caps.italic_support);
    }

    #[test]
    fn default_matches_baseline() {
        let caps = TerminalCaps::default();
        assert!(!caps.true_colour);
        assert!(!caps.kitty_keyboard);
    }
}
