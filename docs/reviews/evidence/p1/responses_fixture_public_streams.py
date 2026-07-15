"""Public SSE fixtures, including explicitly synthetic robustness cases."""

from __future__ import annotations

from typing import Any

from responses_fixture_types import (
    PUBLIC_CACHE_GUIDE,
    PUBLIC_EVENTS,
    PUBLIC_FUNCTION_GUIDE,
    PUBLIC_WEB_GUIDE,
    FixtureSpec,
    event,
    response,
    sentinel,
    stream,
)


def fixture_specs() -> list[FixtureSpec]:
    return [
        _refusal(),
        _message_phase_order(),
        _web_search_annotations(),
        _authoritative_completion(),
        _interleaved_duplicate_calls(),
        _malformed_terminal(),
        _cancellation(),
        _rate_limit_retry(),
        _cache_write_usage(),
        _unknown_actionable(),
        _usage_attempts_and_absence(),
        _retry_after_ceiling(),
        _terminal_once(),
    ]


def _refusal() -> FixtureSpec:
    finding = "EVT-01"
    item_id = sentinel("generic", finding, "message")
    refusal = sentinel("prompt", finding, "refusal")
    return stream(
        finding,
        [
            event(
                "response.refusal.delta",
                1,
                item_id=item_id,
                output_index=0,
                content_index=0,
                delta=refusal,
            ),
            event(
                "response.refusal.done",
                2,
                item_id=item_id,
                output_index=0,
                content_index=0,
                refusal=refusal,
            ),
            event(
                "response.output_item.done",
                3,
                output_index=0,
                item={
                    "id": item_id,
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "refusal", "refusal": refusal}],
                },
            ),
        ],
        "typed-refusal",
        "non-retryable-model-outcome",
    )


def _message_phase_order() -> FixtureSpec:
    finding = "EVT-02"
    phases: list[Any] = [..., None, "commentary", "final_answer"]
    events = []
    sequence = 1
    for index, phase in enumerate(phases):
        item_id = sentinel("generic", finding, f"message-{index}")
        item = {
            "id": item_id,
            "type": "message",
            "role": "assistant",
            "status": "in_progress",
            "content": [],
        }
        if phase is not ...:
            item["phase"] = phase
        events.append(
            event(
                "response.output_item.added",
                sequence,
                output_index=index,
                item=item,
            )
        )
        sequence += 1
        text = sentinel("prompt", finding, f"text-{index}")
        events.append(
            event(
                "response.output_text.delta",
                sequence,
                item_id=item_id,
                output_index=index,
                content_index=0,
                delta=text,
                logprobs=[],
            )
        )
        sequence += 1
        completed = {**item, "status": "completed"}
        completed["content"] = [
            {"type": "output_text", "text": text, "annotations": []}
        ]
        events.append(
            event(
                "response.output_item.done",
                sequence,
                output_index=index,
                item=completed,
            )
        )
        sequence += 1
    return stream(
        finding,
        events,
        "ordered-message-boundaries",
        "phase-absent",
        "phase-null",
        "phase-commentary",
        "phase-final-answer",
    )


def _web_search_annotations() -> FixtureSpec:
    finding = "EVT-03"
    search_id = sentinel("generic", finding, "search")
    message_id = sentinel("generic", finding, "message")
    citation = {
        "type": "url_citation",
        "url": PUBLIC_WEB_GUIDE,
        "title": sentinel("generic", finding, "title"),
        "start_index": 0,
        "end_index": 1,
    }
    search_item = {
        "id": search_id,
        "type": "web_search_call",
        "status": "completed",
        "action": {
            "type": "search",
            "query": sentinel("prompt", finding, "query"),
            "sources": [{"type": "url", "url": PUBLIC_WEB_GUIDE}],
        },
    }
    return stream(
        finding,
        [
            event("response.web_search_call.in_progress", 1, item_id=search_id, output_index=0),
            event("response.web_search_call.searching", 2, item_id=search_id, output_index=0),
            event("response.web_search_call.completed", 3, item_id=search_id, output_index=0),
            event("response.output_item.done", 4, output_index=0, item=search_item),
            event(
                "response.output_text.annotation.added",
                5,
                item_id=message_id,
                output_index=1,
                content_index=0,
                annotation_index=0,
                annotation=citation,
            ),
            event(
                "response.output_item.done",
                6,
                output_index=1,
                item={
                    "id": message_id,
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "content": [
                        {
                            "type": "output_text",
                            "text": sentinel("prompt", finding, "answer"),
                            "annotations": [citation],
                        }
                    ],
                },
            ),
        ],
        "hosted-web-search",
        "url-annotation",
        "search-sources",
        sources=(PUBLIC_EVENTS, PUBLIC_WEB_GUIDE),
    )


def _authoritative_completion() -> FixtureSpec:
    finding = "EVT-04"
    item_id = sentinel("generic", finding, "message")
    delta = sentinel("prompt", finding, "delta")
    authoritative = sentinel("prompt", finding, "authoritative")
    item = {
        "id": item_id,
        "type": "message",
        "role": "assistant",
        "status": "completed",
        "content": [
            {"type": "output_text", "text": authoritative, "annotations": []}
        ],
    }
    return stream(
        finding,
        [
            event(
                "response.output_text.delta",
                1,
                item_id=item_id,
                output_index=0,
                content_index=0,
                delta=delta,
                logprobs=[],
            ),
            event(
                "response.output_text.done",
                2,
                item_id=item_id,
                output_index=0,
                content_index=0,
                text=authoritative,
                logprobs=[],
            ),
            event("response.output_item.done", 3, output_index=0, item=item),
            event(
                "response.completed",
                4,
                response=response(finding, output=[item]),
            ),
        ],
        "delta-authoritative-mismatch",
        "completed-item-reconciliation",
        "terminal-response-reconciliation",
        "synthetic-robustness",
    )


def _interleaved_duplicate_calls() -> FixtureSpec:
    finding = "EVT-06"
    function_item = _call_item(finding, "function_call", "function")
    custom_item = _call_item(finding, "custom_tool_call", "custom")
    function_delta = sentinel("prompt", finding, "function-delta")
    custom_delta = sentinel("prompt", finding, "custom-delta")
    return stream(
        finding,
        [
            event("response.output_item.added", 1, output_index=0, item=function_item),
            event("response.output_item.added", 2, output_index=1, item=custom_item),
            event(
                "response.function_call_arguments.delta",
                3,
                item_id=function_item["id"],
                output_index=0,
                delta=function_delta,
            ),
            event(
                "response.custom_tool_call_input.delta",
                4,
                item_id=custom_item["id"],
                output_index=1,
                delta=custom_delta,
            ),
            event("response.output_item.done", 5, output_index=1, item=custom_item),
            event("response.output_item.done", 6, output_index=0, item=function_item),
            event("response.output_item.done", 7, output_index=0, item=function_item),
        ],
        "function-call",
        "custom-call",
        "interleaved-identities",
        "duplicate-completion",
        "synthetic-robustness",
        sources=(PUBLIC_EVENTS, PUBLIC_FUNCTION_GUIDE),
    )


def _call_item(finding: str, item_type: str, suffix: str) -> dict[str, Any]:
    item = {
        "id": sentinel("generic", finding, f"{suffix}-item"),
        "call_id": sentinel("generic", finding, f"{suffix}-call"),
        "type": item_type,
        "name": sentinel("generic", finding, f"{suffix}-name"),
        "status": "completed",
    }
    if item_type == "function_call":
        item["arguments"] = {"query": sentinel("prompt", finding, suffix)}
    else:
        item["input"] = sentinel("prompt", finding, suffix)
    return item


def _malformed_terminal() -> FixtureSpec:
    finding = "EVT-07"
    return stream(
        finding,
        [event("response.completed", 1, response={"status": "completed"})],
        "malformed-terminal",
        "missing-required-response-fields",
        "synthetic-robustness",
    )


def _cancellation() -> FixtureSpec:
    finding = "TRANS-01"
    return stream(
        finding,
        [event("response.in_progress", 1, response=response(finding, status="in_progress"))],
        "unterminated-in-progress-stream",
        "cancellation-boundary",
        "synthetic-robustness",
    )


def _rate_limit_retry() -> FixtureSpec:
    finding = "TRANS-02"
    return stream(
        finding,
        [
            event(
                "error",
                1,
                code=sentinel("generic", finding, "rate-limit"),
                message=sentinel("prompt", finding, "message"),
                param=None,
            )
        ],
        "standalone-error",
        "in-stream-rate-limit-classification",
        "synthetic-robustness",
    )


def _cache_write_usage() -> FixtureSpec:
    finding = "CACHE-02"
    usage = {
        "input_tokens": 8,
        "input_tokens_details": {"cached_tokens": 5, "cache_write_tokens": 3},
        "output_tokens": 2,
        "output_tokens_details": {"reasoning_tokens": 1},
        "total_tokens": 10,
    }
    return stream(
        finding,
        [event("response.completed", 1, response=response(finding, usage=usage))],
        "cache-write-present-positive",
        "provider-reported-usage",
        sources=(PUBLIC_EVENTS, PUBLIC_CACHE_GUIDE),
    )


def _unknown_actionable() -> FixtureSpec:
    finding = "EVT-05"
    unknown_event = sentinel("generic", finding, "event")
    unknown_item = sentinel("generic", finding, "item-type")
    return stream(
        finding,
        [
            event(unknown_event, 1, item_id=sentinel("generic", finding, "event-item")),
            event(
                "response.output_item.added",
                2,
                output_index=0,
                item={
                    "id": sentinel("generic", finding, "item"),
                    "type": unknown_item,
                    "status": "in_progress",
                },
            ),
        ],
        "unknown-event",
        "unknown-actionable-item",
        "synthetic-robustness",
    )


def _usage_attempts_and_absence() -> FixtureSpec:
    finding = "USAGE-01"
    zero = {
        "input_tokens": 0,
        "input_tokens_details": {"cached_tokens": 0, "cache_write_tokens": 0},
        "output_tokens": 0,
        "output_tokens_details": {"reasoning_tokens": 0},
        "total_tokens": 0,
    }
    return stream(
        finding,
        [
            event("response.failed", 1, response=response(finding, status="failed")),
            event(
                "response.incomplete",
                2,
                response={
                    **response(finding, status="incomplete", usage=None),
                    "incomplete_details": {"reason": "max_output_tokens"},
                },
            ),
            event("response.completed", 3, response=response(finding, usage=zero)),
        ],
        "usage-absent",
        "usage-null",
        "usage-reported-zero",
        "failed-incomplete-successful-attempts",
        "synthetic-robustness",
    )


def _retry_after_ceiling() -> FixtureSpec:
    finding = "NF-3"
    return stream(
        finding,
        [
            event(
                "error",
                1,
                code=sentinel("generic", finding, "rate-limit"),
                message=sentinel("prompt", finding, "message"),
                param=None,
                retry_after=2,
                maximum=1,
            )
        ],
        "retry-after-over-ceiling",
        "synthetic-robustness",
    )


def _terminal_once() -> FixtureSpec:
    finding = "NF-5"
    return stream(
        finding,
        [event("response.failed", 1, response=response(finding, status="failed"))],
        "single-terminal-error",
        "no-trailing-interruption",
    )
