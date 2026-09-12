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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalCaps {
    /// Richest colour encoding supported by the startup evidence.
    pub colour_depth: ColourDepth,
    /// Terminal supports the Kitty keyboard protocol.
    pub kitty_keyboard: bool,
    /// Terminal reports changeable CSI 2026 synchronized output.
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
            let event = terminal.read(Event::is_escape)?;
            caps.observe_reply(&event);
            if matches!(
                event,
                Event::Csi(Csi::Device(csi::Device::DeviceAttributes(())))
            ) {
                // Primary DA is not proof that earlier enhancement replies were received.
                // Drain queued replies; later ones remain with the ordinary input owner.
                timeout = Some(Duration::ZERO);
            }
        }

        Ok(caps)
    }

    /// Reduce a reply at admission or later without turning it into keyboard input.
    pub(super) fn observe_reply(&mut self, event: &Event) {
        match event {
            Event::Csi(Csi::Keyboard(csi::Keyboard::ReportFlags(_))) => {
                self.kitty_keyboard = true;
            }
            Event::Csi(Csi::Mode(csi::Mode::ReportDecPrivateMode {
                mode: DecPrivateMode::Code(DecPrivateModeCode::SynchronizedOutput),
                setting,
            })) => {
                // A frame needs both begin and end. Permanently set/reset modes
                // cannot provide that contract even though the mode is recognized.
                self.synchronized_rendering =
                    matches!(setting, DecModeSetting::Set | DecModeSetting::Reset);
            }
            Event::Csi(Csi::Device(csi::Device::DeviceAttributes(()))) => {
                self.italic_support = true;
            }
            _ => {}
        }
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
    #[test]
    fn progressive_replies_require_toggleable_sync_and_preserve_unrelated_capabilities() {
        let mut caps = TerminalCaps::baseline();
        for (setting, expected) in [
            (DecModeSetting::Reset, true),
            (DecModeSetting::PermanentlyReset, false),
            (DecModeSetting::Set, true),
            (DecModeSetting::PermanentlySet, false),
            (DecModeSetting::NotRecognized, false),
        ] {
            caps.observe_reply(&Event::Csi(Csi::Mode(csi::Mode::ReportDecPrivateMode {
                mode: DecPrivateMode::Code(DecPrivateModeCode::SynchronizedOutput),
                setting,
            })));
            assert_eq!(caps.synchronized_rendering, expected);
        }
        let before = caps.clone();
        caps.observe_reply(&Event::Csi(Csi::Mode(csi::Mode::ReportDecPrivateMode {
            mode: DecPrivateMode::Code(DecPrivateModeCode::AutoWrap),
            setting: DecModeSetting::Set,
        })));
        assert_eq!(caps, before);
        caps.observe_reply(&Event::Csi(Csi::Keyboard(csi::Keyboard::ReportFlags(
            csi::KittyKeyboardFlags::empty(),
        ))));
        assert!(caps.kitty_keyboard);
        caps.observe_reply(&Event::Csi(Csi::Device(csi::Device::DeviceAttributes(()))));
        assert!(caps.italic_support);
        assert_eq!(caps.colour_depth, before.colour_depth);
        assert_eq!(caps.osc_hyperlinks, before.osc_hyperlinks);
        let before = caps.clone();
        caps.observe_reply(&Event::Csi(Csi::Keyboard(csi::Keyboard::ReportFlags(
            csi::KittyKeyboardFlags::empty(),
        ))));
        assert_eq!(caps, before);
    }
}
