//! Three-valued evaluation of Rust conditional-compilation predicates.

use thiserror::Error;

use super::identifier;

/// Reachability of a Rust item when `test` is fixed to `false`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CfgTruth {
    /// The predicate is false in every production configuration.
    False,
    /// The predicate is true in every production configuration.
    True,
    /// The predicate may be true or false in production.
    Possible,
}

impl CfgTruth {
    fn negate(self) -> Self {
        match self {
            Self::False => Self::True,
            Self::True => Self::False,
            Self::Possible => Self::Possible,
        }
    }

    fn all(values: &[Self]) -> Self {
        if values.contains(&Self::False) {
            Self::False
        } else if values.contains(&Self::Possible) {
            Self::Possible
        } else {
            Self::True
        }
    }

    fn any(values: &[Self]) -> Self {
        if values.contains(&Self::True) {
            Self::True
        } else if values.contains(&Self::Possible) {
            Self::Possible
        } else {
            Self::False
        }
    }
}

/// Invalid or unsupported cfg syntax.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum CfgError {
    /// The expression ended before a complete predicate was read.
    #[error("unexpected end of cfg expression")]
    UnexpectedEnd,
    /// The tokenizer found an unsupported character or literal.
    #[error("unsupported cfg syntax at byte {offset}")]
    UnsupportedSyntax {
        /// Byte offset of the unsupported input.
        offset: usize,
    },
    /// The parser found a token other than the one required by the grammar.
    #[error("expected {expected} at byte {offset}")]
    Expected {
        /// Required token description.
        expected: &'static str,
        /// Byte offset of the unexpected token.
        offset: usize,
    },
    /// A cfg function received the wrong number of arguments.
    #[error("cfg function {name} requires {expected} argument(s), found {actual}")]
    Arity {
        /// Function name.
        name: String,
        /// Required argument count.
        expected: usize,
        /// Actual argument count.
        actual: usize,
    },
    /// The predicate used function syntax outside Rust's cfg grammar.
    #[error("unsupported cfg function {name}")]
    UnsupportedFunction {
        /// Unsupported function name.
        name: String,
    },
    /// Tokens remained after the expression was complete.
    #[error("trailing cfg tokens at byte {offset}")]
    TrailingTokens {
        /// Byte offset of the first trailing token.
        offset: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TokenKind {
    Identifier(String),
    String,
    LeftParen,
    RightParen,
    Comma,
    Equals,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Token {
    kind: TokenKind,
    offset: usize,
}

/// Evaluate a cfg predicate with `test` fixed to `false`.
///
/// Target, feature, and other build predicates remain possible. Unsupported
/// or malformed input returns an error rather than being treated as test-only.
pub fn evaluate_cfg(input: &str) -> Result<CfgTruth, CfgError> {
    let tokens = tokenize(input)?;
    let mut parser = Parser { tokens, cursor: 0 };
    let truth = parser.expression()?;
    if parser.cursor != parser.tokens.len() {
        return Err(CfgError::TrailingTokens {
            offset: parser.tokens[parser.cursor].offset,
        });
    }
    Ok(truth)
}

fn tokenize(input: &str) -> Result<Vec<Token>, CfgError> {
    let bytes = input.as_bytes();
    let mut cursor = 0;
    let mut tokens = Vec::new();
    while cursor < bytes.len() {
        if bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
            continue;
        }
        let offset = cursor;
        let kind = match bytes[cursor] {
            b'(' => {
                cursor += 1;
                TokenKind::LeftParen
            }
            b')' => {
                cursor += 1;
                TokenKind::RightParen
            }
            b',' => {
                cursor += 1;
                TokenKind::Comma
            }
            b'=' => {
                cursor += 1;
                TokenKind::Equals
            }
            b'"' => {
                cursor = consume_string(bytes, cursor)?;
                TokenKind::String
            }
            byte if is_identifier_start(byte) => {
                let Some((name, end)) = identifier::token_at(input, cursor) else {
                    return Err(CfgError::UnsupportedSyntax { offset });
                };
                cursor = end;
                TokenKind::Identifier(name.to_owned())
            }
            _ => return Err(CfgError::UnsupportedSyntax { offset }),
        };
        tokens.push(Token { kind, offset });
    }
    Ok(tokens)
}

fn consume_string(bytes: &[u8], start: usize) -> Result<usize, CfgError> {
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => {
                cursor += 2;
                if cursor > bytes.len() {
                    return Err(CfgError::UnexpectedEnd);
                }
            }
            b'"' => return Ok(cursor + 1),
            byte if byte.is_ascii_control() => {
                return Err(CfgError::UnsupportedSyntax { offset: cursor });
            }
            _ => cursor += 1,
        }
    }
    Err(CfgError::UnexpectedEnd)
}

const fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
}

struct CallFrame {
    name: String,
    arguments: Vec<CfgTruth>,
}

impl Parser {
    fn expression(&mut self) -> Result<CfgTruth, CfgError> {
        let mut calls = Vec::new();
        let mut completed = None;
        loop {
            if completed.is_none() {
                let TokenKind::Identifier(name) = self.take()?.kind else {
                    return self.expected("cfg predicate");
                };
                if self.consume(&TokenKind::Equals) {
                    match self.take()?.kind {
                        TokenKind::String => completed = Some(CfgTruth::Possible),
                        _ => return self.expected("string literal"),
                    }
                } else if self.consume(&TokenKind::LeftParen) {
                    if self.consume(&TokenKind::RightParen) {
                        completed = Some(evaluate_call(name, &[])?);
                    } else {
                        calls.push(CallFrame {
                            name,
                            arguments: Vec::new(),
                        });
                        continue;
                    }
                } else {
                    completed = Some(if name == "test" {
                        CfgTruth::False
                    } else {
                        CfgTruth::Possible
                    });
                }
            }

            let value = completed.take().ok_or(CfgError::UnexpectedEnd)?;
            let Some(frame) = calls.last_mut() else {
                return Ok(value);
            };
            frame.arguments.push(value);
            if self.consume(&TokenKind::Comma) && !self.matches(&TokenKind::RightParen) {
                continue;
            }
            self.require(&TokenKind::RightParen, "closing parenthesis")?;
            let frame = calls.pop().ok_or(CfgError::UnexpectedEnd)?;
            completed = Some(evaluate_call(frame.name, &frame.arguments)?);
        }
    }

    fn take(&mut self) -> Result<Token, CfgError> {
        let token = self
            .tokens
            .get(self.cursor)
            .cloned()
            .ok_or(CfgError::UnexpectedEnd)?;
        self.cursor += 1;
        Ok(token)
    }

    fn matches(&self, expected: &TokenKind) -> bool {
        self.tokens
            .get(self.cursor)
            .is_some_and(|token| same_variant(&token.kind, expected))
    }

    fn consume(&mut self, expected: &TokenKind) -> bool {
        if self.matches(expected) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    fn require(&mut self, expected: &TokenKind, description: &'static str) -> Result<(), CfgError> {
        if self.consume(expected) {
            Ok(())
        } else {
            self.expected(description)
        }
    }

    fn expected<T>(&self, expected: &'static str) -> Result<T, CfgError> {
        Err(CfgError::Expected {
            expected,
            offset: self.tokens.get(self.cursor).map_or(0, |token| token.offset),
        })
    }
}

fn evaluate_call(name: String, arguments: &[CfgTruth]) -> Result<CfgTruth, CfgError> {
    match name.as_str() {
        "all" => Ok(CfgTruth::all(arguments)),
        "any" => Ok(CfgTruth::any(arguments)),
        "not" if arguments.len() == 1 => Ok(arguments[0].negate()),
        "not" => Err(CfgError::Arity {
            name,
            expected: 1,
            actual: arguments.len(),
        }),
        _ => Err(CfgError::UnsupportedFunction { name }),
    }
}

const fn same_variant(left: &TokenKind, right: &TokenKind) -> bool {
    matches!(
        (left, right),
        (TokenKind::LeftParen, TokenKind::LeftParen)
            | (TokenKind::RightParen, TokenKind::RightParen)
            | (TokenKind::Comma, TokenKind::Comma)
            | (TokenKind::Equals, TokenKind::Equals)
    )
}
