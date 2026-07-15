"""Shared deterministic types for the sanitized Responses fixture corpus."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

PUBLIC_CREATE = "https://api.openai.com/v1/responses"
PUBLIC_COMPACT = "https://api.openai.com/v1/responses/compact"
PUBLIC_EVENTS = (
    "https://developers.openai.com/api/reference/resources/responses/streaming-events"
)
PUBLIC_TEXT_GUIDE = "https://developers.openai.com/api/docs/guides/text"
PUBLIC_REASONING_GUIDE = "https://developers.openai.com/api/docs/guides/reasoning"
PUBLIC_STATE_GUIDE = "https://developers.openai.com/api/docs/guides/conversation-state"
PUBLIC_COMPACTION_GUIDE = "https://developers.openai.com/api/docs/guides/compaction"
PUBLIC_CACHE_GUIDE = "https://developers.openai.com/api/docs/guides/prompt-caching"
PUBLIC_TOOLS_GUIDE = "https://developers.openai.com/api/docs/guides/tools"
PUBLIC_WEB_GUIDE = "https://developers.openai.com/api/docs/guides/tools-web-search"
PUBLIC_FUNCTION_GUIDE = "https://developers.openai.com/api/docs/guides/function-calling"

CODEX_COMMIT = "0396f99cf1a27fc87dd12d23403b25e840b6ecbd"
CODEX_BASE = f"https://github.com/openai/codex/blob/{CODEX_COMMIT}"
CODEX_CLIENT = f"{CODEX_BASE}/codex-rs/core/src/client.rs"
CODEX_SSE = f"{CODEX_BASE}/codex-rs/codex-api/src/sse/responses.rs"
CODEX_COMMON = f"{CODEX_BASE}/codex-rs/codex-api/src/common.rs"
CODEX_MODELS = f"{CODEX_BASE}/codex-rs/protocol/src/models.rs"
CODEX_LOGIN = f"{CODEX_BASE}/codex-rs/login/src/server.rs"

APPROVED_SOURCE_REFERENCES = frozenset(
    {
        PUBLIC_CREATE,
        PUBLIC_COMPACT,
        PUBLIC_EVENTS,
        PUBLIC_TEXT_GUIDE,
        PUBLIC_REASONING_GUIDE,
        PUBLIC_STATE_GUIDE,
        PUBLIC_COMPACTION_GUIDE,
        PUBLIC_CACHE_GUIDE,
        PUBLIC_TOOLS_GUIDE,
        PUBLIC_WEB_GUIDE,
        PUBLIC_FUNCTION_GUIDE,
        CODEX_CLIENT,
        CODEX_SSE,
        CODEX_COMMON,
        CODEX_MODELS,
        CODEX_LOGIN,
    }
)

Json = dict[str, Any]


@dataclass(frozen=True)
class FixtureSpec:
    """One planned fixture, keyed by its traceability finding."""

    finding_id: str
    dialect: str
    artifact_kind: str
    payload: Json | None
    events: tuple[Json, ...]
    source_references: tuple[str, ...]
    semantic_markers: tuple[str, ...]

    def __post_init__(self) -> None:
        if (self.payload is None) == (not self.events):
            raise ValueError("fixture must contain exactly one payload form")
        if self.artifact_kind == "stream" and not self.events:
            raise ValueError("stream fixture requires events")
        if self.artifact_kind != "stream" and self.payload is None:
            raise ValueError("JSON fixture requires a payload")


def request(
    finding_id: str,
    payload: Json,
    *markers: str,
    dialect: str = "public",
    sources: tuple[str, ...] = (PUBLIC_CREATE,),
) -> FixtureSpec:
    return FixtureSpec(
        finding_id,
        dialect,
        "request",
        payload,
        (),
        sources,
        tuple(markers),
    )


def transport(
    finding_id: str,
    payload: Json,
    *markers: str,
    sources: tuple[str, ...],
) -> FixtureSpec:
    return FixtureSpec(
        finding_id,
        "codex",
        "transport",
        payload,
        (),
        sources,
        tuple(markers),
    )


def stream(
    finding_id: str,
    events: list[Json],
    *markers: str,
    dialect: str = "public",
    sources: tuple[str, ...] = (PUBLIC_EVENTS,),
) -> FixtureSpec:
    return FixtureSpec(
        finding_id,
        dialect,
        "stream",
        None,
        tuple(events),
        sources,
        tuple(markers),
    )


def sentinel(kind: str, finding_id: str, suffix: str = "001") -> str:
    finding = finding_id.lower().replace("_", "-")
    return f"norn-synthetic-{kind}-{finding}-{suffix}"


def message(finding_id: str, role: str, phase: Any = ...) -> Json:
    item: Json = {
        "type": "message",
        "role": role,
        "content": [
            {
                "type": "output_text" if role == "assistant" else "input_text",
                "text": sentinel("prompt", finding_id),
            }
        ],
    }
    if role == "assistant":
        item.update(
            {
                "id": sentinel("generic", finding_id, "message"),
                "status": "completed",
            }
        )
    if phase is not ...:
        item["phase"] = phase
    return item


def response(
    finding_id: str,
    *,
    status: str = "completed",
    output: list[Json] | None = None,
    usage: Json | None | object = ...,
) -> Json:
    value: Json = {
        "id": sentinel("generic", finding_id, "response"),
        "object": "response",
        "status": status,
        "output": output or [],
    }
    if usage is not ...:
        value["usage"] = usage
    return value


def event(event_type: str, sequence: int, **fields: Any) -> Json:
    return {"type": event_type, "sequence_number": sequence, **fields}


def tool_schema(finding_id: str, name_suffix: str = "tool") -> Json:
    field = sentinel("generic", finding_id, "field")
    return {
        "type": "function",
        "name": sentinel("generic", finding_id, name_suffix),
        "description": sentinel("generic", finding_id, "description"),
        "parameters": {
            "type": "object",
            "properties": {field: {"type": "string"}},
            "required": [field],
            "additionalProperties": False,
        },
    }
