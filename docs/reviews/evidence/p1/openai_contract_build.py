"""Deterministic artifact construction for pinned Responses captures."""

from __future__ import annotations

from argparse import Namespace
from pathlib import Path
from typing import Any

from openai_contract_constants import (
    ANNOTATIONS,
    EVENT_COUNT,
    EXPECTED_DISCREPANCIES,
    INCLUDES,
    INCOMPLETE_REASONS,
    INPUT_TYPES,
    OUTPUT_TYPES,
    PHASE_REFS,
    SCHEMA_VERSION,
    STATUSES,
    TOOL_TYPES,
)
from openai_contract_graph import (
    canonical,
    digest,
    event_record,
    example_keys,
    literal_values,
    markdown_sections,
    named_section,
    node_by_oas,
    normalized_source,
    openapi_contract,
    parse_json,
    require_equal,
    root_by_oas,
    sanitized_graph,
    schema_graph,
    shape_record,
    variant_record,
    variants,
)


def build(args: Namespace) -> tuple[dict[str, bytes], dict[str, Any]]:
    raw = {
        name: Path(getattr(args, name)).read_bytes()
        for name in ("create", "compact", "streaming", "websocket")
    }
    text = {name: data.decode("utf-8") for name, data in raw.items()}
    create = parse_json(text["create"], "create OpenAPI")
    compact = parse_json(text["compact"], "compact OpenAPI")
    require_equal(
        (create["openapi"], create["info"]["version"]),
        ("3.1.0", "2.3.0"),
        "create versions",
    )
    require_equal(
        (compact["openapi"], compact["info"]["version"]),
        ("3.1.0", "2.3.0"),
        "compact versions",
    )

    stream_sections = markdown_sections(text["streaming"], 2)
    ws_sections = markdown_sections(text["websocket"], 3, "## Server events")
    require_equal(len(stream_sections), EVENT_COUNT, "stream event count")
    require_equal(
        [name for name, _ in ws_sections],
        [name for name, _ in stream_sections],
        "WebSocket event names",
    )
    events: list[dict[str, Any]] = []
    discrepancies: list[tuple[str, str | None, str]] = []
    stream_graphs: dict[str, dict[str, Any]] = {}
    for (name, section), (_, ws_section) in zip(
        stream_sections, ws_sections, strict=True
    ):
        schema, graph = schema_graph(section, "\n### Example", name)
        ws_schema, ws_graph = schema_graph(
            ws_section, "\n#### Example", f"WebSocket {name}"
        )
        record = event_record(name, schema, graph)
        require_equal(
            event_record(name, ws_schema, ws_graph), record, f"{name} cross-page schema"
        )
        stream_graphs[name] = graph
        keys, invalid = example_keys(section, "\n### Example", name)
        ws_keys, ws_invalid = example_keys(
            ws_section, "\n#### Example", f"WebSocket {name}"
        )
        require_equal(
            (ws_keys, ws_invalid), (keys, invalid), f"{name} cross-page example"
        )
        if invalid:
            discrepancies.append((name, None, "example_invalid_json"))
        else:
            for field in sorted(set(record["required"]) - keys):
                discrepancies.append((name, field, "example_missing_required_field"))
            for field in sorted(keys - set(record["properties"])):
                discrepancies.append((name, field, "example_only_unclassified"))
        events.append(record)
    require_equal(tuple(discrepancies), EXPECTED_DISCREPANCIES, "SSE discrepancies")

    response_schema, response_graph = schema_graph(
        stream_sections[0][1], "\n### Example", "response.created"
    )
    response_root, _ = root_by_oas(
        response_graph, f"#/components/schemas/{response_schema}", "response graph"
    )
    client_section = named_section(text["websocket"], "### response.create", "\n### ")
    request_schema, request_graph = schema_graph(
        client_section, "\n#### Example", "response.create"
    )
    request_root, _ = root_by_oas(
        request_graph,
        "#/components/schemas/ResponsesClientEventResponseCreate/allOf/0",
        "request graph",
    )
    for source_id, graph in (
        ("streaming", response_graph),
        ("websocket", request_graph),
    ):
        for oas_ref in PHASE_REFS:
            _, phase = node_by_oas(graph, "HttpDeclProperty", oas_ref)
            require_equal(
                (phase["optional"], phase["nullable"]),
                (True, True),
                f"{source_id} {oas_ref}",
            )

    _, output_union = root_by_oas(
        response_graph, "#/components/schemas/OutputItem", "output items"
    )
    output = variants(response_graph, output_union["children"])
    _, input_prop = node_by_oas(
        request_graph,
        "HttpDeclProperty",
        "#/components/schemas/CreateResponse/allOf/2/properties/input",
    )
    input_array = request_graph[input_prop["children"][1]]
    inputs = variants(request_graph, input_array["children"])
    _, tools_prop = node_by_oas(
        request_graph,
        "HttpDeclProperty",
        "#/components/schemas/ResponseProperties/properties/tools",
    )
    tools = variants(request_graph, tools_prop["children"])
    _, annotation_prop = node_by_oas(
        response_graph,
        "HttpDeclProperty",
        "#/components/schemas/OutputTextContent/properties/annotations",
    )
    annotations = variants(response_graph, annotation_prop["children"])
    require_equal(
        tuple(item["accepted_literals"][0] for item in inputs),
        INPUT_TYPES,
        "input variants",
    )
    require_equal(
        tuple(item["accepted_literals"][0] for item in output),
        OUTPUT_TYPES,
        "output variants",
    )
    require_equal(
        tuple(tuple(item["accepted_literals"]) for item in tools),
        TOOL_TYPES,
        "tool variants",
    )
    require_equal(
        tuple(item["accepted_literals"][0] for item in annotations),
        ANNOTATIONS,
        "annotations",
    )
    _, include_node = root_by_oas(
        request_graph, "#/components/schemas/IncludeEnum", "include"
    )
    _, status_node = root_by_oas(
        response_graph,
        "#/components/schemas/Response/allOf/2/properties/status",
        "status",
    )
    require_equal(
        tuple(literal_values(include_node["type"])), INCLUDES, "include values"
    )
    require_equal(tuple(literal_values(status_node["type"])), STATUSES, "status values")
    reason_props = [
        value
        for value in response_graph.values()
        if value.get("kind") == "HttpDeclProperty"
        and value.get("key") == "reason"
        and "/incomplete_details/" in str(value.get("oasRef"))
    ]
    require_equal(
        tuple(literal_values(reason_props[0]["type"])),
        INCOMPLETE_REASONS,
        "incomplete reasons",
    )
    reasoning_key = next(
        key
        for key in output_union["children"]
        if "reasoning" in variant_record(response_graph, key, 0)["accepted_literals"]
    )
    compaction_key = next(
        key
        for key in output_union["children"]
        if "compaction" in variant_record(response_graph, key, 0)["accepted_literals"]
    )
    trigger_key = next(
        key
        for key in input_array["children"]
        if "compaction_trigger"
        in variant_record(request_graph, key, 0)["accepted_literals"]
    )

    inventory = {
        "schema_version": SCHEMA_VERSION,
        "kind": "public_responses_inventories",
        "endpoint_contracts": [
            openapi_contract(create, "/responses"),
            openapi_contract(compact, "/responses/compact"),
        ],
        "input_variants": inputs,
        "output_variants": output,
        "tool_variants": tools,
        "annotation_variants": annotations,
        "include_values": list(INCLUDES),
        "response_statuses": list(STATUSES),
        "incomplete_reasons": list(INCOMPLETE_REASONS),
        "usage_paths": [
            "input_tokens",
            "output_tokens",
            "total_tokens",
            "input_tokens_details.cached_tokens",
            "input_tokens_details.cache_write_tokens",
            "output_tokens_details.reasoning_tokens",
        ],
        "cache_controls": {
            "prompt_cache_key": [],
            "prompt_cache_options.mode": ["implicit", "explicit"],
            "prompt_cache_options.ttl": ["30m"],
            "prompt_cache_retention": ["in_memory", "24h"],
            "prompt_cache_breakpoint.mode": ["explicit"],
        },
        "reasoning_item": shape_record(response_graph, reasoning_key),
        "compaction_item": shape_record(response_graph, compaction_key),
        "compaction_trigger": shape_record(request_graph, trigger_key),
        "usage_schema_requires_cache_write_tokens": True,
        "endpoint_examples_may_omit_cache_write_tokens": True,
    }
    event_artifact = {
        "schema_version": SCHEMA_VERSION,
        "kind": "public_responses_sse_events",
        "events": events,
    }
    gate_correction = {
        "id": "gate-a-01",
        "classification": "superseded_gate_a_source_claim",
        "subject": "assistant_message_phase",
        "superseded_claim": "optional_non_nullable",
        "observed_optional": True,
        "observed_nullable": True,
        "source_ids": ["streaming", "websocket"],
        "oas_refs": list(PHASE_REFS),
    }
    discrepancy_artifact = {
        "schema_version": SCHEMA_VERSION,
        "kind": "public_responses_source_discrepancies",
        "count": len(discrepancies),
        "items": [
            {
                "id": f"sse-{index + 1:02d}",
                "event": event,
                "field": field,
                "classification": classification,
            }
            for index, (event, field, classification) in enumerate(discrepancies)
        ],
        "gate_corrections": [gate_correction],
    }
    values = {
        "request-graph.json": sanitized_graph(
            "public_responses_request_graph",
            request_schema,
            request_root,
            request_graph,
        ),
        "response-graph.json": sanitized_graph(
            "public_responses_response_graph",
            response_schema,
            response_root,
            response_graph,
        ),
        "sse-events.json": event_artifact,
        "inventories.json": inventory,
        "source-discrepancies.json": discrepancy_artifact,
    }
    artifacts = {name: canonical(value) for name, value in values.items()}
    sources = []
    source_specs = (
        (
            "create",
            "mcp__openaiDeveloperDocs__get_openapi_spec",
            {"url": "https://api.openai.com/v1/responses"},
        ),
        (
            "compact",
            "mcp__openaiDeveloperDocs__get_openapi_spec",
            {"url": "https://api.openai.com/v1/responses/compact"},
        ),
        (
            "streaming",
            "mcp__openaiDeveloperDocs__fetch_openai_doc",
            {
                "url": "https://developers.openai.com/api/reference/resources/responses/streaming-events"
            },
        ),
        (
            "websocket",
            "mcp__openaiDeveloperDocs__fetch_openai_doc",
            {
                "url": "https://developers.openai.com/api/reference/resources/responses/websocket-events"
            },
        ),
    )
    for source_id, tool, tool_args in source_specs:
        normalized = normalized_source(raw[source_id])
        sources.append(
            {
                "source_id": source_id,
                "tool": tool,
                "arguments": tool_args,
                "raw_bytes": len(raw[source_id]),
                "raw_sha256": digest(raw[source_id]),
                "normalized_bytes": len(normalized),
                "normalized_sha256": digest(normalized),
            }
        )
    metadata = {
        "sources": sources,
        "openapi_version": "3.1.0",
        "api_description_version": "2.3.0",
    }
    return artifacts, metadata
