//! Canonical handling for semantically equivalent Rust identifiers.

/// Return the semantic spelling of one parsed Rust identifier.
pub(crate) fn canonical_bytes(identifier: &[u8]) -> &[u8] {
    identifier.strip_prefix(b"r#").unwrap_or(identifier)
}

/// Return the semantic spelling of one parsed Rust identifier.
pub(crate) fn canonical_text(identifier: &str) -> &str {
    identifier.strip_prefix("r#").unwrap_or(identifier)
}

/// Read one ASCII identifier token, accepting Rust's raw-identifier prefix.
pub(crate) fn token_at(input: &str, start: usize) -> Option<(&str, usize)> {
    let bytes = input.as_bytes();
    let mut cursor = start;
    let identifier_start = if bytes.get(cursor..cursor.checked_add(2)?) == Some(b"r#") {
        cursor += 2;
        cursor
    } else {
        cursor
    };
    if !bytes.get(cursor).copied().is_some_and(is_identifier_start) {
        return None;
    }
    cursor += 1;
    while bytes
        .get(cursor)
        .copied()
        .is_some_and(is_identifier_continue)
    {
        cursor += 1;
    }
    Some((&input[identifier_start..cursor], cursor))
}

/// Strip one exact semantic meta-item name and retain its suffix.
pub(crate) fn name_remainder<'a>(input: &'a str, expected: &str) -> Option<&'a str> {
    let (name, end) = token_at(input, 0)?;
    if name != expected {
        return None;
    }
    let remainder = &input[end..];
    if remainder.is_empty()
        || remainder
            .chars()
            .next()
            .is_some_and(|character| character.is_whitespace() || matches!(character, '(' | '='))
    {
        Some(remainder)
    } else {
        None
    }
}

const fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

const fn is_identifier_continue(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit() || byte == b'-'
}
