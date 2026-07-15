use super::model::SyntheticPurpose;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ScanCode {
    AbsolutePath,
    ControlCharacter,
    DangerousShape,
    ProhibitedField,
    ReusableState,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct RawMatch {
    pub(crate) code: ScanCode,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

pub(crate) fn raw_violations(bytes: &[u8]) -> Vec<RawMatch> {
    let mut matches = Vec::new();
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r') {
            matches.push(RawMatch {
                code: ScanCode::ControlCharacter,
                start: index,
                end: index + 1,
            });
        }
    }
    push_first(
        &mut matches,
        bytes,
        ScanCode::DangerousShape,
        raw_authorization,
    );
    push_first(&mut matches, bytes, ScanCode::DangerousShape, raw_key);
    push_first(
        &mut matches,
        bytes,
        ScanCode::DangerousShape,
        raw_compact_token,
    );
    push_first(&mut matches, bytes, ScanCode::DangerousShape, raw_email);
    push_first(
        &mut matches,
        bytes,
        ScanCode::DangerousShape,
        raw_private_marker,
    );
    push_first(
        &mut matches,
        bytes,
        ScanCode::AbsolutePath,
        raw_private_path,
    );
    matches
}

pub(crate) fn evidence_key_violation(value: &str) -> Option<ScanCode> {
    sensitive_key(value).map(|purpose| match purpose {
        SyntheticPurpose::TurnState | SyntheticPurpose::CacheKey => ScanCode::ReusableState,
        SyntheticPurpose::Generic
        | SyntheticPurpose::AccountId
        | SyntheticPurpose::Credential
        | SyntheticPurpose::PromptContent => ScanCode::ProhibitedField,
    })
}

pub(crate) fn sensitive_key(value: &str) -> Option<SyntheticPurpose> {
    if value.chars().any(char::is_control) {
        return None;
    }
    let key = value.to_ascii_lowercase();
    if matches!(
        key.as_str(),
        "previous_response_id" | "conversation_id" | "turn_state" | "session_state"
    ) {
        return Some(SyntheticPurpose::TurnState);
    }
    if key == "raw_cache_key" || key == "prompt_cache_key" || key.ends_with("_cache_key") {
        return Some(SyntheticPurpose::CacheKey);
    }
    if key.contains("account") || matches!(key.as_str(), "email" | "user_id") {
        return Some(SyntheticPurpose::AccountId);
    }
    if key.contains("authorization")
        || key.contains("credential")
        || key.contains("password")
        || key.contains("secret")
        || key.contains("cookie")
        || matches!(
            key.as_str(),
            "token" | "access_token" | "refresh_token" | "id_token" | "api_key"
        )
    {
        return Some(SyntheticPurpose::Credential);
    }
    if key.contains("prompt")
        || matches!(
            key.as_str(),
            "content" | "text" | "delta" | "refusal" | "input_text" | "output_text"
        )
    {
        return Some(SyntheticPurpose::PromptContent);
    }
    None
}

pub(crate) fn decoded_string_violation(value: &str) -> Option<ScanCode> {
    decoded_structural_violation(value).or_else(|| {
        if has_reusable_state_shape(value) {
            Some(ScanCode::ReusableState)
        } else {
            None
        }
    })
}

pub(crate) fn decoded_structural_violation(value: &str) -> Option<ScanCode> {
    if value.chars().any(char::is_control) {
        return Some(ScanCode::ControlCharacter);
    }
    if raw_private_path(value.as_bytes()).is_some() {
        return Some(ScanCode::AbsolutePath);
    }
    if has_authorization(value)
        || has_key_shape(value)
        || has_compact_token_shape(value)
        || has_email_shape(value)
        || value.contains("norn-private-")
    {
        return Some(ScanCode::DangerousShape);
    }
    None
}

fn push_first(
    matches: &mut Vec<RawMatch>,
    bytes: &[u8],
    code: ScanCode,
    finder: fn(&[u8]) -> Option<(usize, usize)>,
) {
    if let Some((start, end)) = finder(bytes) {
        matches.push(RawMatch { code, start, end });
    }
}

fn raw_authorization(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut scheme = Vec::from(&b"bear"[..]);
    scheme.extend_from_slice(b"er ");
    find_ascii_case_insensitive(bytes, &scheme)
}

fn raw_private_marker(bytes: &[u8]) -> Option<(usize, usize)> {
    find_bytes(bytes, b"norn-private-")
}

fn raw_private_path(bytes: &[u8]) -> Option<(usize, usize)> {
    let unix = [
        b"/Users/".as_slice(),
        b"/home/",
        b"/root/",
        b"/private/",
        b"/var/folders/",
    ]
    .into_iter()
    .find_map(|prefix| find_bytes(bytes, prefix));
    unix.or_else(|| {
        bytes.windows(3).enumerate().find_map(|(start, window)| {
            ((start == 0 || !is_path_word_byte(bytes[start - 1]))
                && window[0].is_ascii_alphabetic()
                && window[1] == b':'
                && matches!(window[2], b'/' | b'\\'))
            .then_some((start, start + window.len()))
        })
    })
}

fn find_bytes(bytes: &[u8], needle: &[u8]) -> Option<(usize, usize)> {
    bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|start| (start, start + needle.len()))
}

fn find_ascii_case_insensitive(bytes: &[u8], needle: &[u8]) -> Option<(usize, usize)> {
    if needle.is_empty() || needle.len() > bytes.len() {
        return None;
    }
    bytes
        .windows(needle.len())
        .position(|window| {
            window
                .iter()
                .zip(needle)
                .all(|(left, right)| left.eq_ignore_ascii_case(right))
        })
        .map(|start| (start, start + needle.len()))
}

fn has_authorization(value: &str) -> bool {
    let mut scheme = Vec::from(&b"bear"[..]);
    scheme.extend_from_slice(b"er ");
    find_ascii_case_insensitive(value.as_bytes(), &scheme).is_some()
}

const fn is_path_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn has_reusable_state_shape(value: &str) -> bool {
    ["resp_", "conv_", "turn_", "sess_", "cache_"]
        .into_iter()
        .any(|prefix| value.starts_with(prefix) && value.len() >= prefix.len() + 8)
}

fn has_compact_token_shape(value: &str) -> bool {
    let mut parts = value.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(second) = parts.next() else {
        return false;
    };
    let Some(third) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && first.starts_with("ey")
        && [first, second, third]
            .into_iter()
            .all(|part| part.len() >= 8 && part.bytes().all(is_url_base64))
}

fn has_key_shape(value: &str) -> bool {
    let prefix = [b's', b'k', b'-'];
    let bytes = value.as_bytes();
    bytes.starts_with(&prefix)
        && bytes.len() >= prefix.len() + 8
        && bytes[prefix.len()..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn is_url_base64(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

fn has_email_shape(value: &str) -> bool {
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    if domain.eq_ignore_ascii_case("example.invalid") {
        return false;
    }
    !local.is_empty()
        && !domain.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+' | b'@')
        })
}

fn raw_compact_token(bytes: &[u8]) -> Option<(usize, usize)> {
    raw_ascii_runs(bytes, |byte| is_url_base64(byte) || byte == b'.').find(|(start, end)| {
        std::str::from_utf8(&bytes[*start..*end]).is_ok_and(has_compact_token_shape)
    })
}

fn raw_key(bytes: &[u8]) -> Option<(usize, usize)> {
    raw_ascii_runs(bytes, |byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
    })
    .find(|(start, end)| std::str::from_utf8(&bytes[*start..*end]).is_ok_and(has_key_shape))
}

fn raw_email(bytes: &[u8]) -> Option<(usize, usize)> {
    raw_ascii_runs(bytes, |byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+' | b'@')
    })
    .find(|(start, end)| std::str::from_utf8(&bytes[*start..*end]).is_ok_and(has_email_shape))
}

fn raw_ascii_runs<'a>(
    bytes: &'a [u8],
    admitted: impl Fn(u8) -> bool + 'a,
) -> impl Iterator<Item = (usize, usize)> + 'a {
    let mut cursor = 0;
    std::iter::from_fn(move || {
        while bytes.get(cursor).is_some_and(|byte| !admitted(*byte)) {
            cursor += 1;
        }
        let start = cursor;
        while bytes.get(cursor).is_some_and(|byte| admitted(*byte)) {
            cursor += 1;
        }
        (start < cursor).then_some((start, cursor))
    })
}
