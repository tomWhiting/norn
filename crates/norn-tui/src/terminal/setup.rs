//! Full-screen terminal ownership with push resize and restoration on every exit path.

use std::io::{self, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use termina::escape::csi::{self, Csi, KittyKeyboardFlags};
use termina::{PlatformTerminal, Terminal};

use super::caps::TerminalCaps;
use crate::TuiError;

const KITTY_FLAGS: KittyKeyboardFlags = KittyKeyboardFlags::from_bits_truncate(
    KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES.bits()
        | KittyKeyboardFlags::REPORT_ALTERNATE_KEYS.bits(),
);
const ENTER_SCREEN: &[u8] = b"\x1b[?1049h\x1b[?7l\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[?25l";
const LEAVE_SCREEN: &[u8] =
    b"\x1b[?2026l\x1b[0m\x1b[?1002l\x1b[?1006l\x1b[?2004l\x1b[?7h\x1b[?25h\x1b[?1049l";

/// Owns raw mode and the alternate screen; no native transcript cursor exists.
pub struct TerminalGuard {
    terminal: PlatformTerminal,
    caps: TerminalCaps,
    columns: u16,
    rows: u16,
    restoration: Arc<AtomicU8>,
}

impl TerminalGuard {
    /// Enter the alternate screen after reading actual terminal geometry.
    /// Partially successful writes are still covered by this guard's restoration.
    pub fn new() -> Result<Self, TuiError> {
        let mut admission = TerminalAdmission {
            terminal: Some(PlatformTerminal::new()?),
        };
        let terminal = admission.terminal_mut()?;
        terminal.enter_raw_mode()?;
        // Guard drop and the panic hook share only restoration ownership. Taking
        // it once prevents an unwind from popping the parent screen's stack.
        let restoration = Arc::new(AtomicU8::new(INACTIVE));
        let panic_restoration = Arc::clone(&restoration);
        terminal.set_panic_hook(move |handle| {
            if let Err(error) = cleanup_owned(handle, &panic_restoration) {
                tracing::error!(%error, "failed to restore retained TUI terminal state during panic");
            }
        });
        let caps = TerminalCaps::detect(terminal)?;
        let dimensions = terminal.get_dimensions()?;
        let mut guard = Self {
            terminal: admission.take()?,
            caps,
            columns: dimensions.cols,
            rows: dimensions.rows,
            restoration,
        };
        enter_screen(
            &mut guard.terminal,
            &guard.restoration,
            guard.caps.kitty_keyboard,
        )?;
        Ok(guard)
    }

    /// Apply dimensions supplied by the terminal's resize event, including zero.
    pub fn handle_resize(&mut self, columns: u16, rows: u16) {
        self.columns = columns;
        self.rows = rows;
    }

    /// Capabilities established at terminal admission.
    #[must_use]
    pub fn caps(&self) -> &TerminalCaps {
        &self.caps
    }

    /// Last observed actual terminal columns.
    #[must_use]
    pub const fn terminal_columns(&self) -> u16 {
        self.columns
    }

    /// Last observed actual terminal rows.
    #[must_use]
    pub const fn terminal_rows(&self) -> u16 {
        self.rows
    }

    /// Terminal output and event-reader owner.
    pub fn terminal_mut(&mut self) -> &mut PlatformTerminal {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if let Err(error) = cleanup_owned(&mut self.terminal, &self.restoration) {
            tracing::error!(%error, "failed to restore retained TUI terminal state");
        }
        if let Err(error) = self.terminal.enter_cooked_mode() {
            tracing::error!(%error, "failed to restore terminal input mode");
        }
    }
}

/// Covers raw-mode admission before dimensions/capabilities are available.
struct TerminalAdmission {
    terminal: Option<PlatformTerminal>,
}

impl TerminalAdmission {
    fn terminal_mut(&mut self) -> io::Result<&mut PlatformTerminal> {
        self.terminal
            .as_mut()
            .ok_or_else(|| io::Error::other("terminal admission ownership already transferred"))
    }

    fn take(&mut self) -> io::Result<PlatformTerminal> {
        self.terminal
            .take()
            .ok_or_else(|| io::Error::other("terminal admission ownership already transferred"))
    }
}

impl Drop for TerminalAdmission {
    fn drop(&mut self) {
        if let Some(terminal) = self.terminal.as_mut()
            && let Err(error) = terminal.enter_cooked_mode()
        {
            tracing::error!(%error, "failed to restore terminal input mode after admission failure");
        }
    }
}

const INACTIVE: u8 = 0;
const SCREEN_ENTERED: u8 = 1;
const KEYBOARD_PUSHED: u8 = 2;

fn enter_screen(
    writer: &mut impl io::Write,
    restoration: &AtomicU8,
    kitty: bool,
) -> io::Result<()> {
    // A partial screen write still needs mode cleanup, but cannot own a keyboard
    // push. Flush the screen switch before allowing a pop on any failure path.
    restoration.store(SCREEN_ENTERED, Ordering::Release);
    writer.write_all(ENTER_SCREEN)?;
    writer.flush()?;
    if kitty {
        // Keyboard stacks are separate for the primary and alternate screens.
        // An uncertain push is safe to pop only after entering the alternate one.
        restoration.store(KEYBOARD_PUSHED, Ordering::Release);
        write!(
            writer,
            "{}",
            Csi::Keyboard(csi::Keyboard::PushFlags(KITTY_FLAGS))
        )?;
        writer.flush()?;
    }
    Ok(())
}

fn cleanup_owned(writer: &mut impl io::Write, restoration: &AtomicU8) -> io::Result<()> {
    match restoration.swap(INACTIVE, Ordering::AcqRel) {
        INACTIVE => Ok(()),
        state => cleanup(writer, state == KEYBOARD_PUSHED),
    }
}

fn cleanup(writer: &mut impl io::Write, kitty: bool) -> io::Result<()> {
    let mut bytes = Vec::new();
    if kitty {
        write!(bytes, "{}", Csi::Keyboard(csi::Keyboard::PopFlags(1)))?;
    }
    bytes.extend_from_slice(LEAVE_SCREEN);
    let write_result = writer.write_all(&bytes);
    let flush_result = writer.flush();
    match (write_result, flush_result) {
        (Err(error), Err(flush_error)) => {
            tracing::error!(%flush_error, "terminal restoration flush also failed");
            Err(error)
        }
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

#[cfg(test)]
#[path = "setup_tests.rs"]
mod tests;
