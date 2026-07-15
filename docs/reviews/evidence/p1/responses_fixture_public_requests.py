"""Public and source-supported Codex request fixtures."""

from __future__ import annotations

from responses_fixture_types import (
    CODEX_CLIENT,
    CODEX_COMMON,
    PUBLIC_CACHE_GUIDE,
    PUBLIC_COMPACT,
    PUBLIC_COMPACTION_GUIDE,
    PUBLIC_CREATE,
    PUBLIC_FUNCTION_GUIDE,
    PUBLIC_REASONING_GUIDE,
    PUBLIC_STATE_GUIDE,
    PUBLIC_TEXT_GUIDE,
    PUBLIC_TOOLS_GUIDE,
    FixtureSpec,
    message,
    request,
    sentinel,
    tool_schema,
)


def fixture_specs() -> list[FixtureSpec]:
    return [
        _stateless_replay(),
        _threaded_replacement(),
        _anchor_reset_reasoning(),
        _role_authority(),
        _slash_tool_dispatch(),
        _cache_experiment(),
        _cache_key_lifecycle(),
        _tool_prefix_stability(),
        _typed_cache_controls(),
        _model_profile(),
        _compatible_roles(),
        _tool_envelopes(),
        _schema_downlevel(),
        _structured_output(),
    ]


def _stateless_replay() -> FixtureSpec:
    finding = "STATE-01"
    reasoning_id = sentinel("generic", finding, "reasoning")
    return request(
        finding,
        {
            "model": sentinel("generic", finding, "model"),
            "store": False,
            "stream": True,
            "include": ["reasoning.encrypted_content"],
            "input": [
                {
                    "type": "reasoning",
                    "id": reasoning_id,
                    "summary": [
                        {
                            "type": "summary_text",
                            "text": sentinel("prompt", finding, "summary"),
                        }
                    ],
                    "encrypted_content": sentinel("generic", finding, "encrypted"),
                    "status": "completed",
                },
                message(finding, "assistant", "commentary"),
                {
                    "type": "function_call",
                    "id": sentinel("generic", finding, "call-item"),
                    "call_id": sentinel("generic", finding, "call"),
                    "name": sentinel("generic", finding, "tool"),
                    "arguments": {"query": sentinel("prompt", finding, "argument")},
                    "status": "completed",
                },
                {
                    "type": "function_call_output",
                    "call_id": sentinel("generic", finding, "call"),
                    "output": sentinel("generic", finding, "output"),
                },
                message(finding, "assistant", "final_answer"),
            ],
        },
        "ordered-output-items",
        "assistant-phase",
        "encrypted-reasoning",
        sources=(PUBLIC_CREATE, PUBLIC_REASONING_GUIDE, PUBLIC_STATE_GUIDE),
    )


def _threaded_replacement() -> FixtureSpec:
    finding = "STATE-02"
    return request(
        finding,
        {
            "model": sentinel("generic", finding, "model"),
            "previous_response_id": sentinel("state", finding),
            "instructions": sentinel("prompt", finding, "replacement"),
            "input": [message(finding, "user")],
            "store": True,
        },
        "stored-continuation",
        "replaceable-instructions",
        sources=(PUBLIC_CREATE, PUBLIC_STATE_GUIDE),
    )


def _anchor_reset_reasoning() -> FixtureSpec:
    finding = "STATE-03"
    return request(
        finding,
        {
            "model": sentinel("generic", finding, "model"),
            "input": [
                {
                    "type": "reasoning",
                    "id": sentinel("generic", finding, "reasoning"),
                    "summary": [],
                    "encrypted_content": sentinel("generic", finding, "encrypted"),
                },
                {
                    "type": "compaction",
                    "id": sentinel("generic", finding, "compaction"),
                    "encrypted_content": sentinel("generic", finding, "compacted"),
                },
                {"type": "compaction_trigger"},
            ],
            "include": ["reasoning.encrypted_content"],
        },
        "compaction",
        "reasoning-continuity",
        sources=(PUBLIC_COMPACT, PUBLIC_COMPACTION_GUIDE, PUBLIC_REASONING_GUIDE),
    )


def _role_authority() -> FixtureSpec:
    finding = "ROLE-01"
    return request(
        finding,
        {
            "model": sentinel("generic", finding, "model"),
            "instructions": sentinel("prompt", finding, "instructions"),
            "input": [
                message(finding, "system"),
                message(finding, "developer"),
                message(finding, "user"),
            ],
        },
        "distinct-authority-roles",
        "top-level-instructions",
        sources=(PUBLIC_CREATE, PUBLIC_TEXT_GUIDE),
    )


def _slash_tool_dispatch() -> FixtureSpec:
    finding = "REQ-01"
    return request(
        finding,
        {
            "model": sentinel("generic", finding, "model"),
            "input": [message(finding, "user")],
            "tools": [tool_schema(finding)],
            "tool_choice": "auto",
        },
        "user-role-command",
        "tool-dispatch-boundary",
        sources=(PUBLIC_CREATE, PUBLIC_TOOLS_GUIDE),
    )


def _cache_experiment() -> FixtureSpec:
    finding = "CACHE-01"
    request_a = {
        "model": sentinel("generic", finding, "model"),
        "prompt_cache_key": sentinel("cache", finding, "shared"),
        "input": [message(finding, "user")],
    }
    request_b = {
        **request_a,
        "input": [message(finding, "user"), message(finding, "developer")],
    }
    return request(
        finding,
        {"requests": [request_a, request_b]},
        "preregistered-cache-experiment",
        "shared-cache-key",
        sources=(PUBLIC_CREATE, PUBLIC_CACHE_GUIDE),
    )


def _cache_key_lifecycle() -> FixtureSpec:
    finding = "CACHE-03"
    return request(
        finding,
        {
            "model": sentinel("generic", finding, "model"),
            "store": False,
            "stream": True,
            "include": ["reasoning.encrypted_content"],
            "prompt_cache_key": sentinel("cache", finding),
            "input": [message(finding, "user")],
            "metadata": {
                sentinel("generic", finding, "metadata-key"): sentinel(
                    "generic", finding, "metadata-value"
                )
            },
        },
        "codex-request-shape",
        "stable-session-cache-key",
        dialect="codex",
        sources=(CODEX_CLIENT, CODEX_COMMON),
    )


def _tool_prefix_stability() -> FixtureSpec:
    finding = "CACHE-04"
    tool = tool_schema(finding)
    return request(
        finding,
        {
            "requests": [
                {"model": sentinel("generic", finding, "model"), "tools": [tool]},
                {"model": sentinel("generic", finding, "model"), "tools": [tool]},
            ]
        },
        "identical-tool-prefix",
        sources=(PUBLIC_CREATE, PUBLIC_CACHE_GUIDE, PUBLIC_TOOLS_GUIDE),
    )


def _typed_cache_controls() -> FixtureSpec:
    finding = "CACHE-05"
    return request(
        finding,
        {
            "model": sentinel("generic", finding, "model"),
            "prompt_cache_key": sentinel("cache", finding),
            "prompt_cache_options": {"mode": "explicit", "ttl": "30m"},
            "prompt_cache_retention": "24h",
            "input": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [
                        {
                            "type": "input_text",
                            "text": sentinel("prompt", finding),
                            "prompt_cache_breakpoint": {"mode": "explicit"},
                        }
                    ],
                }
            ],
        },
        "typed-cache-controls",
        "explicit-breakpoint",
        sources=(PUBLIC_CREATE, PUBLIC_CACHE_GUIDE),
    )


def _model_profile() -> FixtureSpec:
    finding = "MODEL-01"
    return request(
        finding,
        {
            "model": sentinel("generic", finding, "model"),
            "reasoning": {"effort": "high", "summary": "auto"},
            "tools": [tool_schema(finding)],
            "tool_choice": "auto",
            "parallel_tool_calls": True,
            "input": [message(finding, "user")],
        },
        "immutable-model-profile",
        "reasoning-controls",
        sources=(PUBLIC_CREATE, PUBLIC_REASONING_GUIDE, PUBLIC_TOOLS_GUIDE),
    )


def _compatible_roles() -> FixtureSpec:
    finding = "ROLE-02"
    return request(
        finding,
        {
            "model": sentinel("generic", finding, "model"),
            "input": [message(finding, "developer"), message(finding, "user")],
        },
        "developer-role-preserved",
        sources=(PUBLIC_CREATE, PUBLIC_TEXT_GUIDE),
    )


def _tool_envelopes() -> FixtureSpec:
    finding = "TOOL-01"
    return request(
        finding,
        {
            "model": sentinel("generic", finding, "model"),
            "input": [message(finding, "user")],
            "tools": [
                tool_schema(finding, "function"),
                {
                    "type": "custom",
                    "name": sentinel("generic", finding, "custom"),
                    "description": sentinel("generic", finding, "custom-description"),
                },
                {"type": "web_search"},
                {"type": "apply_patch"},
            ],
        },
        "catalog-tool-envelopes",
        "function-and-custom-tools",
        sources=(PUBLIC_CREATE, PUBLIC_TOOLS_GUIDE, PUBLIC_FUNCTION_GUIDE),
    )


def _schema_downlevel() -> FixtureSpec:
    finding = "SCHEMA-01"
    definition = sentinel("generic", finding, "definition")
    return request(
        finding,
        {
            "model": sentinel("generic", finding, "model"),
            "input": [message(finding, "user")],
            "tools": [
                {
                    "type": "function",
                    "name": sentinel("generic", finding, "tool"),
                    "description": sentinel("generic", finding, "description"),
                    "parameters": {
                        "type": "object",
                        "$defs": {definition: {"type": "string"}},
                        "properties": {definition: {"$ref": f"#/$defs/{definition}"}},
                        "required": [definition],
                        "additionalProperties": False,
                    },
                }
            ],
        },
        "local-reference-preservation",
        "schema-downlevel-boundary",
        sources=(PUBLIC_CREATE, PUBLIC_FUNCTION_GUIDE),
    )


def _structured_output() -> FixtureSpec:
    finding = "STRUCT-01"
    field = sentinel("generic", finding, "field")
    return request(
        finding,
        {
            "model": sentinel("generic", finding, "model"),
            "input": [message(finding, "user")],
            "text": {
                "format": {
                    "type": "json_schema",
                    "name": sentinel("generic", finding, "format"),
                    "schema": {
                        "type": "object",
                        "properties": {field: {"type": "string"}},
                        "required": [field],
                        "additionalProperties": False,
                    },
                }
            },
        },
        "responses-native-structured-output",
        sources=(PUBLIC_CREATE,),
    )
