//! Closed lexer for the Rust meta-item subset used by policy attributes.

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Token {
    pub(super) kind: TokenKind,
    pub(super) start: usize,
    pub(super) end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum TokenKind {
    Identifier(String),
    Literal(Vec<u8>),
    Open,
    Close,
    Comma,
    Equal,
    PathSeparator,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum TokenTag {
    Open,
    Close,
    Comma,
    Equal,
    PathSeparator,
}

impl TokenTag {
    pub(super) const fn matches(self, kind: &TokenKind) -> bool {
        matches!(
            (self, kind),
            (Self::Open, TokenKind::Open)
                | (Self::Close, TokenKind::Close)
                | (Self::Comma, TokenKind::Comma)
                | (Self::Equal, TokenKind::Equal)
                | (Self::PathSeparator, TokenKind::PathSeparator)
        )
    }
}

pub(super) fn lex(body: &str) -> Result<Vec<Token>, usize> {
    let bytes = body.as_bytes();
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
            continue;
        }
        let start = cursor;
        let kind = match bytes[cursor] {
            b'(' => single(&mut cursor, TokenKind::Open),
            b')' => single(&mut cursor, TokenKind::Close),
            b',' => single(&mut cursor, TokenKind::Comma),
            b'=' => single(&mut cursor, TokenKind::Equal),
            b':' if bytes.get(cursor + 1) == Some(&b':') => {
                cursor += 2;
                TokenKind::PathSeparator
            }
            b'"' => TokenKind::Literal(lex_cooked(bytes, &mut cursor, LiteralClass::String)?),
            b'\'' => TokenKind::Literal(lex_cooked(bytes, &mut cursor, LiteralClass::Character)?),
            b'b' if matches!(bytes.get(cursor + 1), Some(b'"' | b'\'')) => {
                cursor += 1;
                let class = if bytes.get(cursor) == Some(&b'"') {
                    LiteralClass::ByteString
                } else {
                    LiteralClass::ByteCharacter
                };
                TokenKind::Literal(lex_cooked(bytes, &mut cursor, class)?)
            }
            b'r' | b'b' if raw_literal_info(bytes, cursor).is_some() => {
                TokenKind::Literal(lex_raw(bytes, &mut cursor)?)
            }
            b'r' if is_raw_identifier(bytes, cursor) => {
                cursor += 2;
                TokenKind::Identifier(lex_identifier(bytes, &mut cursor)?)
            }
            byte if is_identifier_start(byte) => {
                TokenKind::Identifier(lex_identifier(bytes, &mut cursor)?)
            }
            byte if byte.is_ascii_digit() || byte == b'-' => {
                cursor += 1;
                while bytes.get(cursor).is_some_and(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'+' | b'-')
                }) {
                    cursor += 1;
                }
                TokenKind::Literal(bytes[start..cursor].to_vec())
            }
            _ => return Err(cursor),
        };
        tokens.push(Token {
            kind,
            start,
            end: cursor,
        });
    }
    Ok(tokens)
}

fn single(cursor: &mut usize, kind: TokenKind) -> TokenKind {
    *cursor += 1;
    kind
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum LiteralClass {
    String,
    ByteString,
    Character,
    ByteCharacter,
}

impl LiteralClass {
    const fn tag(self) -> &'static [u8] {
        match self {
            Self::String => b"string",
            Self::ByteString => b"byte_string",
            Self::Character => b"character",
            Self::ByteCharacter => b"byte_character",
        }
    }

    const fn permits_unicode(self) -> bool {
        matches!(self, Self::String | Self::Character)
    }

    const fn permits_continuation(self) -> bool {
        matches!(self, Self::String | Self::ByteString)
    }
}

fn lex_identifier(bytes: &[u8], cursor: &mut usize) -> Result<String, usize> {
    let start = *cursor;
    if !bytes.get(*cursor).copied().is_some_and(is_identifier_start) {
        return Err(start);
    }
    *cursor += 1;
    while bytes
        .get(*cursor)
        .copied()
        .is_some_and(is_identifier_continue)
    {
        *cursor += 1;
    }
    let Ok(value) = std::str::from_utf8(&bytes[start..*cursor]) else {
        return Err(start);
    };
    Ok(value.to_owned())
}

fn is_raw_identifier(bytes: &[u8], start: usize) -> bool {
    bytes.get(start..start + 2) == Some(b"r#")
        && bytes
            .get(start + 2)
            .copied()
            .is_some_and(is_identifier_start)
}

fn lex_cooked(bytes: &[u8], cursor: &mut usize, class: LiteralClass) -> Result<Vec<u8>, usize> {
    let start = *cursor;
    let quote = bytes[*cursor];
    *cursor += 1;
    let mut value = Vec::new();
    while let Some(byte) = bytes.get(*cursor).copied() {
        if byte == quote {
            *cursor += 1;
            validate_literal_value(class, &value, start)?;
            return Ok(canonical_literal(class, &value));
        }
        if byte == b'\\' {
            *cursor += 1;
            decode_escape(bytes, cursor, class, &mut value, start)?;
            continue;
        }
        if matches!(byte, b'\n' | b'\r') || (!class.permits_unicode() && !byte.is_ascii()) {
            return Err(*cursor);
        }
        value.push(byte);
        *cursor += 1;
    }
    Err(start)
}

fn decode_escape(
    bytes: &[u8],
    cursor: &mut usize,
    class: LiteralClass,
    value: &mut Vec<u8>,
    start: usize,
) -> Result<(), usize> {
    let Some(escaped) = bytes.get(*cursor).copied() else {
        return Err(start);
    };
    *cursor += 1;
    match escaped {
        b'\\' | b'\'' | b'"' => value.push(escaped),
        b'n' => value.push(b'\n'),
        b'r' => value.push(b'\r'),
        b't' => value.push(b'\t'),
        b'0' => value.push(0),
        b'x' => decode_hex_byte(bytes, cursor, class, value)?,
        b'u' if class.permits_unicode() => decode_unicode(bytes, cursor, value)?,
        b'\n' if class.permits_continuation() => skip_continuation_whitespace(bytes, cursor),
        b'\r' if class.permits_continuation() && bytes.get(*cursor).copied() == Some(b'\n') => {
            *cursor += 1;
            skip_continuation_whitespace(bytes, cursor);
        }
        _ => return Err(cursor.saturating_sub(1)),
    }
    Ok(())
}

fn decode_hex_byte(
    bytes: &[u8],
    cursor: &mut usize,
    class: LiteralClass,
    value: &mut Vec<u8>,
) -> Result<(), usize> {
    let offset = *cursor;
    let Some(high) = bytes.get(*cursor).copied().and_then(hex_value) else {
        return Err(offset);
    };
    let Some(low) = bytes.get(*cursor + 1).copied().and_then(hex_value) else {
        return Err(offset);
    };
    let decoded = high * 16 + low;
    if class.permits_unicode() && !decoded.is_ascii() {
        return Err(offset);
    }
    value.push(decoded);
    *cursor += 2;
    Ok(())
}

fn decode_unicode(bytes: &[u8], cursor: &mut usize, value: &mut Vec<u8>) -> Result<(), usize> {
    let offset = *cursor;
    if bytes.get(*cursor).copied() != Some(b'{') {
        return Err(offset);
    }
    *cursor += 1;
    let mut decoded = 0_u32;
    let mut digits = 0_u8;
    loop {
        let Some(byte) = bytes.get(*cursor).copied() else {
            return Err(offset);
        };
        *cursor += 1;
        if byte == b'}' {
            break;
        }
        if byte == b'_' {
            continue;
        }
        let Some(nibble) = hex_value(byte) else {
            return Err(cursor.saturating_sub(1));
        };
        digits = digits.saturating_add(1);
        if digits > 6 {
            return Err(offset);
        }
        decoded = decoded
            .checked_mul(16)
            .and_then(|current| current.checked_add(u32::from(nibble)))
            .ok_or(offset)?;
    }
    if digits == 0 {
        return Err(offset);
    }
    let Some(character) = char::from_u32(decoded) else {
        return Err(offset);
    };
    let mut encoded = [0_u8; 4];
    value.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
    Ok(())
}

fn skip_continuation_whitespace(bytes: &[u8], cursor: &mut usize) {
    while bytes
        .get(*cursor)
        .copied()
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        *cursor += 1;
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn validate_literal_value(class: LiteralClass, value: &[u8], offset: usize) -> Result<(), usize> {
    match class {
        LiteralClass::String if std::str::from_utf8(value).is_ok() => Ok(()),
        LiteralClass::Character => {
            let Ok(text) = std::str::from_utf8(value) else {
                return Err(offset);
            };
            let mut characters = text.chars();
            let first = characters.next();
            if first.is_none() || characters.next().is_some() {
                return Err(offset);
            }
            Ok(())
        }
        LiteralClass::ByteString => Ok(()),
        LiteralClass::ByteCharacter if value.len() == 1 => Ok(()),
        LiteralClass::String | LiteralClass::ByteCharacter => Err(offset),
    }
}

fn canonical_literal(class: LiteralClass, value: &[u8]) -> Vec<u8> {
    let mut canonical = Vec::with_capacity(class.tag().len() + value.len() + 17);
    canonical.extend_from_slice(class.tag());
    canonical.push(0);
    let length = value.len().to_be_bytes();
    canonical.extend_from_slice(&[0_u8; 16][length.len()..]);
    canonical.extend_from_slice(&length);
    canonical.extend_from_slice(value);
    canonical
}

fn raw_literal_info(bytes: &[u8], start: usize) -> Option<(usize, usize, LiteralClass)> {
    let mut cursor = start;
    let class = if bytes.get(cursor) == Some(&b'b') {
        cursor += 1;
        LiteralClass::ByteString
    } else {
        LiteralClass::String
    };
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let hash_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    (bytes.get(cursor) == Some(&b'"')).then_some((cursor + 1, cursor - hash_start, class))
}

fn lex_raw(bytes: &[u8], cursor: &mut usize) -> Result<Vec<u8>, usize> {
    let start = *cursor;
    let Some((content_start, hashes, class)) = raw_literal_info(bytes, start) else {
        return Err(start);
    };
    *cursor = content_start;
    while *cursor < bytes.len() {
        let hash_end = *cursor + 1 + hashes;
        if bytes[*cursor] == b'"'
            && bytes
                .get(*cursor + 1..hash_end)
                .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
        {
            let value = &bytes[content_start..*cursor];
            if class == LiteralClass::ByteString && !value.is_ascii() {
                return Err(start);
            }
            *cursor = hash_end;
            return Ok(canonical_literal(class, value));
        }
        *cursor += 1;
    }
    Err(start)
}

const fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

const fn is_identifier_continue(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}
