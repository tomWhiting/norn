//! Parse one MCP executable and argument vector without executing a shell.

use super::NornWrappedClaudeError;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ParsedCommandLine {
    pub(super) command: String,
    pub(super) args: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Quote {
    None,
    Single,
    Double,
}

pub(super) fn parse_command_line(input: &str) -> Result<ParsedCommandLine, NornWrappedClaudeError> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = Quote::None;
    let mut word_started = false;
    let mut characters = input.chars();

    while let Some(character) = characters.next() {
        match (quote, character) {
            (Quote::None, '\'') => {
                quote = Quote::Single;
                word_started = true;
            }
            (Quote::None, '"') => {
                quote = Quote::Double;
                word_started = true;
            }
            (Quote::None, '\\') => {
                let escaped = characters
                    .next()
                    .ok_or(NornWrappedClaudeError::TrailingMcpCommandEscape)?;
                if escaped != '\n' {
                    current.push(escaped);
                    word_started = true;
                }
            }
            (Quote::None, character) if character.is_whitespace() => {
                if word_started {
                    words.push(std::mem::take(&mut current));
                    word_started = false;
                }
            }
            (Quote::Single, '\'') | (Quote::Double, '"') => quote = Quote::None,
            (Quote::Double, '\\') => {
                let escaped = characters
                    .next()
                    .ok_or(NornWrappedClaudeError::TrailingMcpCommandEscape)?;
                match escaped {
                    '"' | '\\' | '$' | '`' => current.push(escaped),
                    '\n' => {}
                    other => {
                        current.push('\\');
                        current.push(other);
                    }
                }
                if escaped != '\n' {
                    word_started = true;
                }
            }
            (_, character) => {
                current.push(character);
                word_started = true;
            }
        }
    }

    match quote {
        Quote::Single => return Err(NornWrappedClaudeError::UnterminatedMcpSingleQuote),
        Quote::Double => return Err(NornWrappedClaudeError::UnterminatedMcpDoubleQuote),
        Quote::None => {}
    }
    if word_started {
        words.push(current);
    }

    let mut words = words.into_iter();
    let command = words
        .next()
        .ok_or(NornWrappedClaudeError::EmptyMcpCommand)?;
    if command.is_empty() {
        return Err(NornWrappedClaudeError::EmptyMcpCommand);
    }
    Ok(ParsedCommandLine {
        command,
        args: words.collect(),
    })
}
