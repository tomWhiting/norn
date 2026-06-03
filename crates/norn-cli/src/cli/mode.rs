//! Execution-mode detection for the Norn CLI (NC-001 R3).
//!
//! Per `DESIGN.md` NC5: the binary runs as either an interactive REPL or a
//! one-shot print invocation. The decision is made up-front from three
//! inputs: the explicit `--print` flag, whether stdin is connected to a
//! terminal, and whether stdout is connected to a terminal. Keeping this
//! function pure (booleans in, enum out) makes every branch unit-testable
//! without touching real file descriptors.

/// Mode the Norn CLI runs in for a single invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Terminal user interface with DECSTBM scroll regions.
    Tui,
    /// Headless, single-shot execution suitable for shell pipelines and
    /// automation.
    Print,
}

/// Decide the execution mode from the user's flags and the I/O environment.
///
/// Print mode wins when any of the following is true:
///
/// 1. `print_flag` is set (explicit `--print` / `-p`).
/// 2. stdin is not a TTY (piped input).
/// 3. stdout is not a TTY (piped output).
///
/// Otherwise the CLI launches the TUI. Accepting the TTY
/// state as parameters (rather than calling [`std::io::IsTerminal`]
/// internally) keeps the function deterministic and trivially testable.
#[must_use]
pub fn detect_mode(print_flag: bool, stdin_is_tty: bool, stdout_is_tty: bool) -> Mode {
    if print_flag || !stdin_is_tty || !stdout_is_tty {
        Mode::Print
    } else {
        Mode::Tui
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn print_flag_forces_print_mode_regardless_of_tty() {
        assert_eq!(detect_mode(true, true, true), Mode::Print);
        assert_eq!(detect_mode(true, false, true), Mode::Print);
        assert_eq!(detect_mode(true, true, false), Mode::Print);
        assert_eq!(detect_mode(true, false, false), Mode::Print);
    }

    #[test]
    fn piped_stdin_selects_print_mode() {
        assert_eq!(detect_mode(false, false, true), Mode::Print);
    }

    #[test]
    fn piped_stdout_selects_print_mode() {
        assert_eq!(detect_mode(false, true, false), Mode::Print);
    }

    #[test]
    fn both_ttys_no_flag_selects_tui() {
        assert_eq!(detect_mode(false, true, true), Mode::Tui);
    }

    #[test]
    fn both_pipes_no_flag_selects_print() {
        assert_eq!(detect_mode(false, false, false), Mode::Print);
    }
}
