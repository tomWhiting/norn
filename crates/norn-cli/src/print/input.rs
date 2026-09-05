//! Piped-stdin reading and prompt composition for print-mode execution.

use std::io::{IsTerminal, Read};

use super::error::PrintError;

/// Read stdin in full when it is not a TTY. Returns [`None`] when stdin
/// is a TTY (print mode invoked from a terminal with `-p`).
pub(super) fn read_stdin_if_piped() -> Result<Option<String>, PrintError> {
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(None);
    }
    let mut buf = String::new();
    stdin.lock().read_to_string(&mut buf)?;
    Ok(Some(buf))
}

/// Build the effective prompt given an optional piped-stdin payload and
/// the positional `PROMPT` words joined into a single string.
///
/// Logic per NC-003 R4:
/// - `stdin = None`: return the positional prompt verbatim.
/// - `stdin = Some`, positional empty: use stdin verbatim.
/// - both present: wrap stdin in `<stdin>…</stdin>` and concatenate.
#[must_use]
pub fn compose_prompt(stdin: Option<&str>, positional: &str) -> String {
    match (stdin, positional.is_empty()) {
        (None, _) => positional.to_owned(),
        (Some(content), true) => content.to_owned(),
        (Some(content), false) => {
            format!("<stdin>\n{content}\n</stdin>\n\n{positional}")
        }
    }
}
