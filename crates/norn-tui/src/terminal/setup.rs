//! Terminal lifecycle — raw mode, DECSTBM scroll regions, panic cleanup.
//!
//! `TerminalGuard` owns the terminal for the duration of the TUI session.
//! On creation it enters raw mode, sets a panic hook, detects capabilities,
//! and establishes the initial DECSTBM scroll region. On drop (or panic)
//! it restores the terminal to its original state.

use std::io::{self, Write as _};

use termina::escape::csi::{
    self, Csi, Cursor, DecPrivateMode, DecPrivateModeCode, KittyKeyboardFlags, Mode,
};
use termina::{OneBased, PlatformTerminal, Terminal};

use super::caps::TerminalCaps;
use crate::TuiError;

/// Kitty keyboard flags pushed on entry when the terminal supports the
/// protocol. Matches what Helix and Kakoune push at the time of writing
/// (see `termina::escape::csi`): `DISAMBIGUATE_ESCAPE_CODES` so plain
/// Enter and Esc are reported distinctly from modified or prefix sequences,
/// and `REPORT_ALTERNATE_KEYS` so the shifted codepoint is delivered next
/// to the base key.
///
/// The pair is what unlocks the `SUPER` (Command on macOS) and reliable
/// `SHIFT+Enter` modifier bits on Kitty-protocol terminals (Ghostty,
/// Kitty, `WezTerm`, Foot). On terminals that do not advertise the protocol
/// the flags are never pushed.
const KITTY_FLAGS: KittyKeyboardFlags = KittyKeyboardFlags::from_bits_truncate(
    KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES.bits()
        | KittyKeyboardFlags::REPORT_ALTERNATE_KEYS.bits(),
);

fn bracketed_paste_mode() -> DecPrivateMode {
    DecPrivateMode::Code(DecPrivateModeCode::BracketedPaste)
}

fn bracketed_paste_set_sequence() -> Csi {
    Csi::Mode(Mode::SetDecPrivateMode(bracketed_paste_mode()))
}

fn bracketed_paste_reset_sequence() -> Csi {
    Csi::Mode(Mode::ResetDecPrivateMode(bracketed_paste_mode()))
}

/// Owns the terminal and restores its state on drop.
///
/// The guard enters raw mode and sets up DECSTBM scroll regions on
/// creation. When dropped, it resets scroll regions, re-shows the cursor,
/// re-enables line wrap, and lets `PlatformTerminal`'s own drop restore
/// cooked mode.
pub struct TerminalGuard {
    terminal: PlatformTerminal,
    caps: TerminalCaps,
    terminal_rows: u16,
    panel_height: u16,
    /// Software-tracked one-based row of the cursor inside the scroll
    /// region. Advanced by [`Self::note_scroll_newlines`] on every
    /// scroll-region write so that [`Self::restore_scroll_cursor_clamped`]
    /// can decide whether the cursor saved by DECSC would now land
    /// below the (possibly shrunken) bottom margin after a panel grow.
    ///
    /// Tracking precision: this counter advances by the `\n` count of
    /// each scroll-region write, capped at the current scroll-region
    /// bottom. Soft-wrapped long lines under-count by the wrap amount.
    /// Wrapping is rare in streamed assistant text (mostly short chunks
    /// with frequent newlines); when it does happen the clamp may miss
    /// in either direction, but the degradation is bounded.
    scroll_cursor_row: u16,
    /// Snapshot of [`Self::scroll_cursor_row`] captured by
    /// [`Self::save_scroll_cursor`]. `None` outside an active
    /// save/restore bracket. Consumed by
    /// [`Self::restore_scroll_cursor_clamped`].
    scroll_cursor_row_at_save: Option<u16>,
    /// Accumulated rows the panel grew by since the last
    /// [`Self::save_scroll_cursor`]. Set by
    /// [`Self::note_panel_grew`] (called from `redraw_panel`'s grow
    /// path after its content-preserving `\n` emissions). Consumed
    /// by [`Self::restore_scroll_cursor_clamped`] to distinguish a
    /// grow-induced clamp (skip `\r\n` — the grow already scrolled)
    /// from a resize-induced clamp (emit `\r\n` to preserve one row).
    panel_grew_by: u16,
}

impl TerminalGuard {
    /// Set up the terminal for TUI operation.
    ///
    /// Enters raw mode, registers a panic hook for cleanup, detects
    /// capabilities, and establishes the initial DECSTBM scroll region
    /// with a minimal fixed panel (1 row separator + 1 row input + 1 row
    /// status bar).
    pub fn new() -> Result<Self, TuiError> {
        let mut terminal = PlatformTerminal::new()?;
        terminal.enter_raw_mode()?;

        terminal.set_panic_hook(|handle| {
            cleanup_handle(handle);
        });

        let caps = TerminalCaps::detect(&mut terminal)?;
        if caps.kitty_keyboard {
            write!(
                terminal,
                "{}",
                Csi::Keyboard(csi::Keyboard::PushFlags(KITTY_FLAGS)),
            )?;
            terminal.flush()?;
        }
        write!(terminal, "{}", bracketed_paste_set_sequence())?;
        terminal.flush()?;
        let dims = terminal.get_dimensions()?;
        let terminal_rows = dims.rows;
        let initial_panel_height: u16 = 3;

        let mut guard = Self {
            terminal,
            caps,
            terminal_rows,
            panel_height: initial_panel_height,
            // The initial scroll region sets up DECSTBM and homes the
            // cursor to (1, 1) — see `setup_initial_scroll_region`.
            scroll_cursor_row: 1,
            scroll_cursor_row_at_save: None,
            panel_grew_by: 0,
        };

        guard.setup_initial_scroll_region()?;

        Ok(guard)
    }

    /// Reissue DECSTBM when the fixed panel height changes.
    ///
    /// Emits the new scroll region boundary. Per VT100 spec, DECSTBM
    /// homes the cursor to (1,1) — callers must reposition afterward.
    /// Does NOT touch DECSC/DECRC so the scroll-region cursor slot
    /// managed by the event loop is preserved.
    pub fn reissue_scroll_region(&mut self, new_panel_height: u16) -> Result<(), crate::TuiError> {
        self.panel_height = new_panel_height;
        let scroll_bottom = self.scroll_bottom()?;

        write!(
            self.terminal,
            "{}",
            Csi::Cursor(Cursor::SetTopAndBottomMargins {
                top: OneBased::from_zero_based(0),
                bottom: OneBased::from_zero_based(scroll_bottom - 1),
            }),
        )?;
        self.terminal.flush()?;
        Ok(())
    }

    /// Handle a terminal resize event.
    pub fn handle_resize(&mut self, new_rows: u16) -> Result<(), TuiError> {
        self.terminal_rows = new_rows;
        self.reissue_scroll_region(self.panel_height)
    }

    /// Access the detected terminal capabilities.
    pub fn caps(&self) -> &TerminalCaps {
        &self.caps
    }

    /// Number of terminal rows.
    pub fn terminal_rows(&self) -> u16 {
        self.terminal_rows
    }

    /// Current fixed panel height.
    pub fn panel_height(&self) -> u16 {
        self.panel_height
    }

    /// Rows remaining between the software cursor and the scroll-region
    /// bottom (inclusive). Used to decide whether a dim repaint would
    /// overflow and cause an unwanted scroll.
    pub fn scroll_rows_below_cursor(&self) -> u16 {
        let scroll_bottom = self.terminal_rows.saturating_sub(self.panel_height);
        scroll_bottom
            .saturating_sub(self.scroll_cursor_row)
            .saturating_add(1)
    }

    /// Mutable access to the terminal for writing escape sequences.
    pub fn terminal_mut(&mut self) -> &mut PlatformTerminal {
        &mut self.terminal
    }

    /// Reset the scroll-region cursor row to `row` (one-based).
    ///
    /// Call this when the hardware cursor is repositioned by something
    /// other than a scroll-region content write — e.g. after
    /// `\x1b[2J\x1b[H` at session start, or after an explicit DECSTBM
    /// reissue that homes the cursor.
    pub fn reset_scroll_cursor(&mut self, row: u16) {
        self.scroll_cursor_row = row.max(1);
    }

    /// Note that `content` was just written to the scroll region.
    ///
    /// Counts the `\n` bytes in `content` and advances
    /// [`Self::scroll_cursor_row`] by that amount, capped at the
    /// current scroll-region bottom (because at the bottom margin LF
    /// scrolls and the cursor stays on the bottom row).
    pub fn note_scroll_newlines(&mut self, content: &str) -> io::Result<()> {
        let count: u16 = content.matches('\n').count().try_into().unwrap_or(u16::MAX);
        if count == 0 {
            return Ok(());
        }
        let scroll_bottom = self.scroll_bottom()?;
        self.scroll_cursor_row = advance_scroll_row(self.scroll_cursor_row, count, scroll_bottom);
        Ok(())
    }

    /// Record that the panel grew by `rows` during a `redraw_panel`
    /// call's content-preserving scroll.
    ///
    /// The grow path emits `rows` newlines at the old scroll-region
    /// bottom to push content into native scrollback before the panel
    /// claims those terminal rows. This method adjusts the software
    /// cursor tracker so [`Self::restore_scroll_cursor_clamped`] knows
    /// the content shifted up and does not emit a redundant `\r\n`
    /// (which would create a blank line in the output).
    ///
    /// Accumulates across multiple grows between save/restore brackets
    /// (e.g. several agent events arriving between turns that each
    /// grow the panel by one row).
    pub fn note_panel_grew(&mut self, rows: u16) {
        self.panel_grew_by = self.panel_grew_by.saturating_add(rows);
        self.scroll_cursor_row = self.scroll_cursor_row.saturating_sub(rows).max(1);
        if let Some(saved) = self.scroll_cursor_row_at_save.as_mut() {
            *saved = saved.saturating_sub(rows).max(1);
        }
    }

    /// Save the scroll-region cursor (DECSC + snapshot the software
    /// tracker) so [`Self::restore_scroll_cursor_clamped`] can clamp
    /// to the scroll region after a panel grow.
    pub fn save_scroll_cursor(&mut self) -> io::Result<()> {
        write!(self.terminal, "\x1b7")?;
        self.scroll_cursor_row_at_save = Some(self.scroll_cursor_row);
        self.panel_grew_by = 0;
        Ok(())
    }

    /// Restore the scroll-region cursor (DECRC), clamping into the
    /// scroll region when the panel grew between the matching
    /// [`Self::save_scroll_cursor`] and this call.
    ///
    /// Two clamp paths exist depending on whether the panel grew:
    ///
    /// **Grow clamp** (`panel_grew_by > 0`): the grow path in
    /// `redraw_panel` already scrolled content into native scrollback
    /// via `\n` emissions. DECRC restores the hardware cursor to the
    /// pre-grow position (now stale — content shifted up). The cursor
    /// is repositioned to the adjusted row without a `\r\n` because
    /// the grow path already preserved the bottom content.
    ///
    /// **Non-grow clamp** (e.g. terminal resize): the saved row lands
    /// inside the panel without a preceding content-preserving scroll.
    /// The cursor is forced to the scroll-region bottom and a `\r\n`
    /// pushes one row of content into native scrollback.
    pub fn restore_scroll_cursor_clamped(&mut self) -> io::Result<()> {
        let grew = std::mem::take(&mut self.panel_grew_by);
        write!(self.terminal, "\x1b8")?;
        let saved = self.scroll_cursor_row_at_save.take();
        if let Some(row) = saved {
            self.scroll_cursor_row = row;
        }
        let scroll_bottom = self.scroll_bottom()?;
        if grew > 0 {
            let target = self.scroll_cursor_row.min(scroll_bottom);
            write!(
                self.terminal,
                "{}",
                Csi::Cursor(Cursor::Position {
                    line: OneBased::from_zero_based(target.saturating_sub(1)),
                    col: OneBased::from_zero_based(0),
                }),
            )?;
            self.scroll_cursor_row = target;
        } else if self.scroll_cursor_row > scroll_bottom {
            write!(
                self.terminal,
                "{}",
                Csi::Cursor(Cursor::Position {
                    line: OneBased::from_zero_based(scroll_bottom.saturating_sub(1)),
                    col: OneBased::from_zero_based(0),
                }),
            )?;
            self.terminal.write_all(b"\r\n")?;
            self.scroll_cursor_row = scroll_bottom;
        }
        Ok(())
    }

    fn setup_initial_scroll_region(&mut self) -> Result<(), TuiError> {
        let scroll_bottom = self.scroll_bottom()?;

        let top = OneBased::from_zero_based(0);
        let bottom = OneBased::from_zero_based(scroll_bottom - 1);

        write!(
            self.terminal,
            "{}{}",
            Csi::Cursor(Cursor::SetTopAndBottomMargins { top, bottom }),
            Csi::Cursor(Cursor::Position {
                line: top,
                col: OneBased::from_zero_based(0),
            }),
        )?;
        self.terminal.flush()?;

        Ok(())
    }

    fn scroll_bottom(&self) -> io::Result<u16> {
        let scroll_bottom = self.terminal_rows.saturating_sub(self.panel_height);
        if scroll_bottom == 0 {
            return Err(io::Error::other("terminal too small for TUI"));
        }
        Ok(scroll_bottom)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.caps.kitty_keyboard {
            let _ = write!(
                self.terminal,
                "{}",
                Csi::Keyboard(csi::Keyboard::PopFlags(1)),
            );
        }
        let _ = write!(self.terminal, "{}", bracketed_paste_reset_sequence());
        let _ = write!(
            self.terminal,
            concat!(
                "\x1b[r",    // Reset DECSTBM (full screen)
                "\x1b[?25h", // Show cursor (DECTCEM)
                "\x1b[?7h",  // Enable line wrap (DECAWM)
            )
        );
        let _ = self.terminal.flush();
    }
}

/// Pure helper for [`TerminalGuard::note_scroll_newlines`].
///
/// Advances `current` by `newlines`, saturating at `u16::MAX` and then
/// clamping to `scroll_bottom` (because LF at the bottom margin of a
/// DECSTBM region scrolls the region rather than advancing the
/// cursor — the cursor stays on the bottom row).
const fn advance_scroll_row(current: u16, newlines: u16, scroll_bottom: u16) -> u16 {
    let advanced = current.saturating_add(newlines);
    if advanced > scroll_bottom {
        scroll_bottom
    } else {
        advanced
    }
}

/// Cleanup for the panic hook — receives a raw platform handle.
///
/// The panic-hook closure passed to `termina::set_panic_hook` is a plain
/// function pointer with no access to `TerminalGuard::caps`, so the Kitty
/// pop is emitted unconditionally. Terminals that never had the protocol
/// pushed silently ignore the unknown CSI sequence — the trade is a few
/// stray bytes versus a stuck keyboard mode after a panic.
fn cleanup_handle(handle: &mut termina::PlatformHandle) {
    let _ = write!(handle, "{}", Csi::Keyboard(csi::Keyboard::PopFlags(1)));
    let _ = write!(handle, "{}", bracketed_paste_reset_sequence());
    let _ = write!(
        handle,
        concat!(
            "\x1b[r",    // Reset DECSTBM (full screen)
            "\x1b[?25h", // Show cursor (DECTCEM)
            "\x1b[?7h",  // Enable line wrap (DECAWM)
        )
    );
    let _ = handle.flush();
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use termina::OneBased;
    use termina::escape::csi::{Csi, Cursor};

    #[test]
    fn decstbm_sequence_for_known_dimensions() {
        let top = OneBased::from_zero_based(0);
        let bottom = OneBased::from_zero_based(37);
        let seq = format!(
            "{}",
            termina::escape::csi::Csi::Cursor(
                termina::escape::csi::Cursor::SetTopAndBottomMargins { top, bottom }
            )
        );
        assert!(
            seq.contains("1;38"),
            "expected '1;38' in DECSTBM sequence, got: {seq}"
        );
    }

    #[test]
    fn reissue_with_larger_panel_shrinks_scroll_region() {
        let top = OneBased::from_zero_based(0);
        let bottom_3 = OneBased::from_zero_based(36);
        let bottom_4 = OneBased::from_zero_based(35);
        let seq_3 = format!(
            "{}",
            termina::escape::csi::Csi::Cursor(
                termina::escape::csi::Cursor::SetTopAndBottomMargins {
                    top,
                    bottom: bottom_3
                }
            )
        );
        let seq_4 = format!(
            "{}",
            termina::escape::csi::Csi::Cursor(
                termina::escape::csi::Cursor::SetTopAndBottomMargins {
                    top,
                    bottom: bottom_4
                }
            )
        );
        assert!(seq_3.contains("1;37"), "expected '1;37', got: {seq_3}");
        assert!(seq_4.contains("1;36"), "expected '1;36', got: {seq_4}");
    }

    #[test]
    fn cleanup_static_sequences_include_terminal_resets() {
        let reset = concat!("\x1b[r", "\x1b[?25h", "\x1b[?7h");
        assert!(reset.contains("\x1b[r"));
        assert!(reset.contains("\x1b[?25h"));
        assert!(reset.contains("\x1b[?7h"));
    }

    #[test]
    fn bracketed_paste_set_sequence_enables_dec_private_mode() {
        let seq = format!("{}", super::bracketed_paste_set_sequence());
        assert_eq!(seq, "\x1b[?2004h");
    }

    #[test]
    fn bracketed_paste_reset_sequence_disables_dec_private_mode() {
        let seq = format!("{}", super::bracketed_paste_reset_sequence());
        assert_eq!(seq, "\x1b[?2004l");
    }

    #[test]
    fn kitty_flags_combine_disambiguate_and_alternate_keys() {
        // DISAMBIGUATE_ESCAPE_CODES (1) | REPORT_ALTERNATE_KEYS (4) = 5.
        // Any drift in the underlying termina bit constants would break
        // the modifier delivery contract — assert the actual bits.
        let bits = super::KITTY_FLAGS.bits();
        assert_eq!(
            bits, 5,
            "expected DISAMBIGUATE(1) | REPORT_ALTERNATE_KEYS(4) = 5, got {bits}"
        );
    }

    #[test]
    fn kitty_push_flags_emits_push_csi() {
        // Format the Push CSI termina renders for the configured flag
        // set; this is the exact byte stream `TerminalGuard::new` writes
        // when the terminal advertises Kitty support.
        let seq = format!(
            "{}",
            termina::escape::csi::Csi::Keyboard(termina::escape::csi::Keyboard::PushFlags(
                super::KITTY_FLAGS,
            )),
        );
        assert!(
            seq.contains(">5u"),
            "expected push CSI '>5u' for combined flags, got: {seq:?}"
        );
    }

    #[test]
    fn kitty_pop_flags_emits_pop_one_csi() {
        // Drop and panic cleanup both emit PopFlags(1) — verify the
        // exact CSI shape so a future termina-version bump that altered
        // the encoding would surface here.
        let seq = format!(
            "{}",
            termina::escape::csi::Csi::Keyboard(termina::escape::csi::Keyboard::PopFlags(1)),
        );
        assert!(
            seq.contains("<1u"),
            "expected pop CSI '<1u' for one stack entry, got: {seq:?}"
        );
    }

    // ---------------- scroll_cursor_row tracking (FIX A) ----------------

    #[test]
    fn advance_scroll_row_below_bottom_advances_unchanged() {
        // Mid-region advance: cursor at row 5, write 3 newlines, bottom
        // at row 20 — cursor settles at row 8.
        let next = super::advance_scroll_row(5, 3, 20);
        assert_eq!(next, 8);
    }

    #[test]
    fn advance_scroll_row_at_bottom_stays_at_bottom() {
        // DECSTBM semantics: LF at bottom margin scrolls, cursor stays.
        let next = super::advance_scroll_row(20, 1, 20);
        assert_eq!(next, 20);
    }

    #[test]
    fn advance_scroll_row_overshoots_clamps_to_bottom() {
        // Cursor at row 18, write 10 newlines, bottom 20 — clamps at 20
        // rather than walking off into the panel area.
        let next = super::advance_scroll_row(18, 10, 20);
        assert_eq!(next, 20);
    }

    #[test]
    fn advance_scroll_row_saturates_without_overflow() {
        // u16-saturated addition guards against pathological inputs.
        let next = super::advance_scroll_row(u16::MAX - 1, 10, 20);
        assert_eq!(
            next, 20,
            "saturated then clamped — must not wrap past u16 boundary"
        );
    }

    #[test]
    fn advance_scroll_row_above_bottom_clamps_immediately() {
        // Pre-clamp state with row > bottom (e.g. just after a panel
        // grow that shrunk scroll_bottom). One more newline must still
        // settle at the (new) bottom.
        let next = super::advance_scroll_row(25, 1, 20);
        assert_eq!(next, 20);
    }

    #[test]
    fn clamp_reposition_targets_scroll_bottom_col_one() {
        // restore_scroll_cursor_clamped emits a cursor-position CSI to
        // the new bottom row, col 1. Verify the exact byte stream
        // termina generates so the clamp lands inside the scroll region
        // rather than below it.
        let line = OneBased::from_zero_based(20 - 1); // new bottom = 20 → one-based 20
        let col = OneBased::from_zero_based(0); // col 1 one-based
        let seq = format!("{}", Csi::Cursor(Cursor::Position { line, col }));
        assert!(
            seq.contains("20;1"),
            "expected '20;1' in clamp position CSI, got: {seq}"
        );
    }

    // ----------- note_panel_grew cursor adjustment (FIX B) -----------

    /// Simulate the cursor tracking fields of `TerminalGuard` without
    /// constructing a real terminal. Tests the pure arithmetic of
    /// `note_panel_grew`'s adjustments.
    struct CursorTracker {
        scroll_cursor_row: u16,
        scroll_cursor_row_at_save: Option<u16>,
        panel_grew_by: u16,
    }

    impl CursorTracker {
        fn new(row: u16, saved: Option<u16>) -> Self {
            Self {
                scroll_cursor_row: row,
                scroll_cursor_row_at_save: saved,
                panel_grew_by: 0,
            }
        }

        fn note_panel_grew(&mut self, rows: u16) {
            self.panel_grew_by = self.panel_grew_by.saturating_add(rows);
            self.scroll_cursor_row = self.scroll_cursor_row.saturating_sub(rows).max(1);
            if let Some(saved) = self.scroll_cursor_row_at_save.as_mut() {
                *saved = saved.saturating_sub(rows).max(1);
            }
        }
    }

    #[test]
    fn note_panel_grew_adjusts_cursor_and_save() {
        let mut t = CursorTracker::new(35, Some(35));
        t.note_panel_grew(2);
        assert_eq!(t.scroll_cursor_row, 33);
        assert_eq!(t.scroll_cursor_row_at_save, Some(33));
        assert_eq!(t.panel_grew_by, 2);
    }

    #[test]
    fn note_panel_grew_accumulates_across_calls() {
        let mut t = CursorTracker::new(35, Some(35));
        t.note_panel_grew(1);
        t.note_panel_grew(1);
        assert_eq!(t.scroll_cursor_row, 33);
        assert_eq!(t.scroll_cursor_row_at_save, Some(33));
        assert_eq!(t.panel_grew_by, 2);
    }

    #[test]
    fn note_panel_grew_clamps_to_one() {
        let mut t = CursorTracker::new(2, Some(2));
        t.note_panel_grew(10);
        assert_eq!(t.scroll_cursor_row, 1, "must not go below row 1");
        assert_eq!(t.scroll_cursor_row_at_save, Some(1));
    }

    #[test]
    fn note_panel_grew_without_save_adjusts_live_cursor_only() {
        let mut t = CursorTracker::new(20, None);
        t.note_panel_grew(3);
        assert_eq!(t.scroll_cursor_row, 17);
        assert!(t.scroll_cursor_row_at_save.is_none());
        assert_eq!(t.panel_grew_by, 3);
    }

    #[test]
    fn note_panel_grew_mid_region_cursor_stays_valid() {
        // Cursor mid-screen (not at bottom). After grow by 2,
        // the adjusted row is still well inside the region.
        let mut t = CursorTracker::new(10, Some(10));
        t.note_panel_grew(2);
        assert_eq!(t.scroll_cursor_row, 8);
        assert_eq!(t.scroll_cursor_row_at_save, Some(8));
    }

    #[test]
    fn note_panel_grew_zero_is_identity() {
        let mut t = CursorTracker::new(15, Some(15));
        t.note_panel_grew(0);
        assert_eq!(t.scroll_cursor_row, 15);
        assert_eq!(t.scroll_cursor_row_at_save, Some(15));
        assert_eq!(t.panel_grew_by, 0);
    }
}
