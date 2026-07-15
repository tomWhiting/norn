//! Exact decoding for Rust string literals used as repository paths.

pub(super) fn decode_rust_string(input: &str) -> Option<String> {
    let input = input.trim();
    if input.starts_with('r') {
        decode_raw(input)
    } else {
        decode_cooked(input)
    }
}

fn decode_raw(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    if bytes.first().copied()? != b'r' {
        return None;
    }
    let mut quote = 1;
    while bytes.get(quote).copied() == Some(b'#') {
        quote += 1;
    }
    if bytes.get(quote).copied() != Some(b'"') {
        return None;
    }
    let hashes = quote - 1;
    let suffix_len = 1 + hashes;
    if bytes.len() < quote + 1 + suffix_len || bytes[bytes.len() - suffix_len] != b'"' {
        return None;
    }
    if !bytes[bytes.len() - hashes..]
        .iter()
        .all(|byte| *byte == b'#')
    {
        return None;
    }
    Some(input[quote + 1..bytes.len() - suffix_len].to_owned())
}

fn decode_cooked(input: &str) -> Option<String> {
    let body = input.strip_prefix('"')?.strip_suffix('"')?;
    let bytes = body.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'\\' {
            output.push(bytes[cursor]);
            cursor += 1;
            continue;
        }
        cursor += 1;
        match bytes.get(cursor).copied()? {
            b'n' => output.push(b'\n'),
            b'r' => output.push(b'\r'),
            b't' => output.push(b'\t'),
            b'0' => output.push(0),
            b'\\' => output.push(b'\\'),
            b'"' => output.push(b'"'),
            b'\'' => output.push(b'\''),
            b'x' => {
                let high = hex(*bytes.get(cursor + 1)?)?;
                let low = hex(*bytes.get(cursor + 2)?)?;
                output.push((high << 4) | low);
                cursor += 2;
            }
            b'u' => {
                let (character, end) = unicode_escape(bytes, cursor + 1)?;
                let mut encoded = [0_u8; 4];
                output.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
                cursor = end;
            }
            b'\n' => {
                cursor += 1;
                while bytes
                    .get(cursor)
                    .copied()
                    .is_some_and(|byte| byte.is_ascii_whitespace())
                {
                    cursor += 1;
                }
                continue;
            }
            _ => return None,
        }
        cursor += 1;
    }
    let Ok(output) = String::from_utf8(output) else {
        return None;
    };
    Some(output)
}

fn unicode_escape(bytes: &[u8], opening: usize) -> Option<(char, usize)> {
    if bytes.get(opening).copied()? != b'{' {
        return None;
    }
    let mut cursor = opening + 1;
    let mut value = 0_u32;
    let mut digits = 0_u8;
    while let Some(byte) = bytes.get(cursor).copied() {
        if byte == b'}' {
            return if digits == 0 {
                None
            } else {
                char::from_u32(value).map(|character| (character, cursor))
            };
        }
        if byte != b'_' {
            value = value.checked_mul(16)?.checked_add(u32::from(hex(byte)?))?;
            digits = digits.checked_add(1)?;
            if digits > 6 {
                return None;
            }
        }
        cursor += 1;
    }
    None
}

const fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
