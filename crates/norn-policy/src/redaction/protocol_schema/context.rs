use crate::path::RepositoryPath;

use super::super::model::PublicUrl;

const PUBLIC_GUIDE_URLS: &[&str] = &[
    "https://developers.openai.com/api/docs/guides/compaction",
    "https://developers.openai.com/api/docs/guides/conversation-state",
    "https://developers.openai.com/api/docs/guides/function-calling",
    "https://developers.openai.com/api/docs/guides/prompt-caching",
    "https://developers.openai.com/api/docs/guides/reasoning",
    "https://developers.openai.com/api/docs/guides/text",
    "https://developers.openai.com/api/docs/guides/tools",
    "https://developers.openai.com/api/docs/guides/tools-web-search",
];

const CODEX_SOURCE_URLS: &[&str] = &[
    "https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/codex-api/src/common.rs",
    "https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/codex-api/src/sse/responses.rs",
    "https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/core/src/client.rs",
    "https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/login/src/server.rs",
    "https://github.com/openai/codex/blob/0396f99cf1a27fc87dd12d23403b25e840b6ecbd/codex-rs/protocol/src/models.rs",
];

const PINNED_SOURCE_PATHS: &[&str] = &[
    "codex-rs/codex-api/src/common.rs",
    "codex-rs/codex-api/src/sse/responses.rs",
    "codex-rs/core/src/client.rs",
    "codex-rs/login/src/server.rs",
    "codex-rs/protocol/src/models.rs",
    "crates/norn/testdata/openai_responses/codex/manifest.json",
    "crates/norn/testdata/openai_responses/public/manifest.json",
    "policy/contracts/openai-responses-v1/manifest.json",
];

pub(in crate::redaction) fn is_approved_source_url(value: &str) -> bool {
    (value != PublicUrl::OpenAiWebsocketEvents.as_str() && public_contract_urls().contains(&value))
        || PUBLIC_GUIDE_URLS.contains(&value)
        || CODEX_SOURCE_URLS.contains(&value)
}

pub(in crate::redaction) fn is_fixed_url(value: &str) -> bool {
    if public_contract_urls().contains(&value) || PUBLIC_GUIDE_URLS.contains(&value) {
        return true;
    }
    let Some(authority_and_path) = value.strip_prefix("https://") else {
        return false;
    };
    let host = authority_and_path
        .split_once('/')
        .map_or(authority_and_path, |(host, _)| host);
    host == "example.invalid" || host.ends_with(".example.invalid")
}

pub(in crate::redaction) fn is_fixture_path(value: &str) -> bool {
    let Ok(path) = RepositoryPath::parse(value) else {
        return false;
    };
    path.as_str()
        .strip_prefix("crates/norn/testdata/openai_responses/")
        .is_some_and(|suffix| !suffix.is_empty() && is_path_suffix(suffix))
}

pub(in crate::redaction) fn is_pinned_source_path(value: &str) -> bool {
    PINNED_SOURCE_PATHS.contains(&value)
}

pub(in crate::redaction) fn is_fixture_id(value: &str) -> bool {
    value
        .strip_prefix("fixture-")
        .is_some_and(is_lower_machine_component)
}

pub(in crate::redaction) fn is_category(value: &str) -> bool {
    value.split_once('/').is_some_and(|(family, subject)| {
        !subject.contains('/')
            && is_lower_machine_component(family)
            && is_lower_machine_component(subject)
    })
}

pub(in crate::redaction) fn is_finding_id(value: &str) -> bool {
    value.split_once('-').is_some_and(|(family, ordinal)| {
        !family.is_empty()
            && family.bytes().all(|byte| byte.is_ascii_uppercase())
            && !ordinal.is_empty()
            && ordinal.bytes().all(|byte| byte.is_ascii_digit())
    })
}

pub(in crate::redaction) fn is_concern(value: &str) -> bool {
    matches!(
        value,
        "request_authority"
            | "stored_continuation"
            | "stateless_continuation"
            | "assistant_phase"
            | "turn_state"
            | "completion"
            | "metadata"
            | "compaction"
            | "error_retry_semantics"
            | "cache_reporting"
    )
}

pub(in crate::redaction) fn is_hex_pin(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

pub(in crate::redaction) fn is_json_pointer(value: &str) -> bool {
    value.starts_with("#/")
        && value.len() > 2
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'#' | b'/' | b'~' | b'.' | b'_' | b'-' | b'$')
        })
}

const fn public_contract_urls() -> [&'static str; 4] {
    [
        PublicUrl::OpenAiResponsesEndpoint.as_str(),
        PublicUrl::OpenAiCompactEndpoint.as_str(),
        PublicUrl::OpenAiStreamingEvents.as_str(),
        PublicUrl::OpenAiWebsocketEvents.as_str(),
    ]
}

fn is_lower_machine_component(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn is_path_suffix(value: &str) -> bool {
    value.bytes().all(|byte| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'/' | b'.' | b'_' | b'-')
    })
}
