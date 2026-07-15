const PUBLIC_FIXTURE_SOURCES: [&str; 11] = [
    "https://api.openai.com/v1/responses",
    "https://api.openai.com/v1/responses/compact",
    "https://developers.openai.com/api/reference/resources/responses/streaming-events",
    "https://developers.openai.com/api/docs/guides/text",
    "https://developers.openai.com/api/docs/guides/reasoning",
    "https://developers.openai.com/api/docs/guides/conversation-state",
    "https://developers.openai.com/api/docs/guides/compaction",
    "https://developers.openai.com/api/docs/guides/prompt-caching",
    "https://developers.openai.com/api/docs/guides/tools",
    "https://developers.openai.com/api/docs/guides/tools-web-search",
    "https://developers.openai.com/api/docs/guides/function-calling",
];

const PUBLIC_EXTRACTION_SOURCES: [&str; 4] = [
    "https://api.openai.com/v1/responses",
    "https://api.openai.com/v1/responses/compact",
    "https://developers.openai.com/api/reference/resources/responses/streaming-events",
    "https://developers.openai.com/api/reference/resources/responses/websocket-events",
];

const CODEX_SOURCES: [&str; 5] = [
    concat!(
        "https://github.com/openai/codex/blob/",
        "0396f99cf1a27fc87dd12d23403b25e840b6ecbd/",
        "codex-rs/core/src/client.rs"
    ),
    concat!(
        "https://github.com/openai/codex/blob/",
        "0396f99cf1a27fc87dd12d23403b25e840b6ecbd/",
        "codex-rs/codex-api/src/sse/responses.rs"
    ),
    concat!(
        "https://github.com/openai/codex/blob/",
        "0396f99cf1a27fc87dd12d23403b25e840b6ecbd/",
        "codex-rs/codex-api/src/common.rs"
    ),
    concat!(
        "https://github.com/openai/codex/blob/",
        "0396f99cf1a27fc87dd12d23403b25e840b6ecbd/",
        "codex-rs/protocol/src/models.rs"
    ),
    concat!(
        "https://github.com/openai/codex/blob/",
        "0396f99cf1a27fc87dd12d23403b25e840b6ecbd/",
        "codex-rs/login/src/server.rs"
    ),
];

pub(super) fn is_fixture_source(value: &str) -> bool {
    PUBLIC_FIXTURE_SOURCES.contains(&value) || CODEX_SOURCES.contains(&value)
}

pub(super) fn is_public_extraction_source(value: &str) -> bool {
    PUBLIC_EXTRACTION_SOURCES.contains(&value)
}
