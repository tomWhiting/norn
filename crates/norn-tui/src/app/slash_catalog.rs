//! TUI slash-command parsing plus the TUI view of the shared built-in
//! slash catalog.

pub(super) use norn::agent_loop::{EffortCommand, effort_label, parse_effort_command};

use norn::agent_loop::{
    BuiltinSlashCommand, BuiltinSlashKind, SlashSurface, builtin_slash_commands,
    find_builtin_slash_command,
};

/// Side-effecting handler kind for a TUI builtin.
pub(super) type TuiBuiltinKind = BuiltinSlashKind;

/// TUI builtin slash-command metadata shared by help and autocomplete.
pub(super) type TuiBuiltinCommand = BuiltinSlashCommand;

/// Builtin slash commands handled directly by the TUI.
pub(super) fn tui_builtin_commands() -> impl Iterator<Item = &'static TuiBuiltinCommand> {
    builtin_slash_commands(SlashSurface::Tui)
}

pub(super) fn find_tui_builtin_command(name: &str) -> Option<&'static TuiBuiltinCommand> {
    find_builtin_slash_command(SlashSurface::Tui, name)
}

/// Classification result for [`classify_slash`].
///
/// Separates the parse-and-recognise step from the do-the-work step so
/// matching logic can be unit-tested without constructing a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SlashClass<'a> {
    /// Text is not a slash command at all.
    NotSlash,
    /// Text is `/` followed by whitespace or nothing.
    Empty,
    /// Slash command name and trimmed argument tail.
    Recognised {
        /// Command name as typed.
        cmd: &'a str,
        /// Trimmed argument tail.
        arg: &'a str,
    },
}

/// Parse `text` against the TUI slash grammar.
pub(super) fn classify_slash(text: &str) -> SlashClass<'_> {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix('/') else {
        return SlashClass::NotSlash;
    };
    let (cmd, arg) = split_first_word(rest);
    if cmd.is_empty() {
        return SlashClass::Empty;
    }
    SlashClass::Recognised { cmd, arg }
}

/// Split `s` on the first whitespace run.
pub(super) fn split_first_word(s: &str) -> (&str, &str) {
    let trimmed = s.trim_start();
    match trimmed.find(char::is_whitespace) {
        Some(idx) => (&trimmed[..idx], trimmed[idx..].trim()),
        None => (trimmed, ""),
    }
}

/// Whether `cmd` (without leading slash) is a TUI builtin.
#[cfg(test)]
pub(super) fn is_tui_builtin(cmd: &str) -> bool {
    find_tui_builtin_command(cmd).is_some()
}
