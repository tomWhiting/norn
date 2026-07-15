"""Closed parsing and graph-sanitization helpers for Responses captures."""

from __future__ import annotations

import hashlib
import json
import re
from typing import Any

from openai_contract_constants import NODE_KEYS, SCHEMA_VERSION, TYPE_KEYS


class ContractError(ValueError):
    """The source or checked contract violates its closed format."""


def reject_duplicate_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ContractError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def parse_json(text: str, label: str) -> Any:
    try:
        return json.loads(text, object_pairs_hook=reject_duplicate_pairs)
    except (json.JSONDecodeError, ContractError) as error:
        raise ContractError(f"{label}: invalid JSON: {error}") from error


def canonical(value: Any) -> bytes:
    return (
        json.dumps(value, indent=2, sort_keys=True, ensure_ascii=True) + "\n"
    ).encode()


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def normalized_source(data: bytes) -> bytes:
    return data.replace(b"\r\n", b"\n").replace(b"\r", b"\n")


def markdown_sections(
    markdown: str, level: int, after: str | None = None
) -> list[tuple[str, str]]:
    start = markdown.index(after) if after is not None else 0
    fragment = markdown[start:]
    matches = list(re.finditer(rf"(?m)^{'#' * level} ([^\n]+)$", fragment))
    return [
        (
            match.group(1),
            fragment[
                match.start() : matches[index + 1].start()
                if index + 1 < len(matches)
                else len(fragment)
            ],
        )
        for index, match in enumerate(matches)
    ]


def named_section(markdown: str, heading: str, next_heading: str) -> str:
    start = markdown.index(heading)
    end = markdown.find(next_heading, start + len(heading))
    return markdown[start : end if end >= 0 else len(markdown)]


def schema_graph(
    section: str, example_heading: str, label: str
) -> tuple[str, dict[str, Any]]:
    match = re.search(r"Schema name: `([^`]+)`", section)
    if match is None:
        raise ContractError(f"{label}: schema name is missing")
    start = section.find("```json", match.end())
    example = section.find(example_heading, start)
    end = section.rfind("\n```", start, example)
    if min(start, example, end) < 0:
        raise ContractError(f"{label}: schema fence is incomplete")
    graph = parse_json(section[start + 7 : end].strip(), f"{label} schema")
    if not isinstance(graph, dict):
        raise ContractError(f"{label}: schema graph must be an object")
    return match.group(1), graph


def example_keys(
    section: str, example_heading: str, label: str
) -> tuple[set[str] | None, bool]:
    marker = section.find(example_heading)
    start = section.find("```json", marker)
    end = section.find("\n```", start + 7)
    if min(marker, start, end) < 0:
        raise ContractError(f"{label}: example fence is incomplete")
    try:
        value = parse_json(section[start + 7 : end].strip(), f"{label} example")
    except ContractError:
        return None, True
    if not isinstance(value, dict):
        raise ContractError(f"{label}: example must be an object")
    return set(value), False


def checked_keys(value: dict[str, Any], allowed: set[str], label: str) -> None:
    unknown = set(value) - allowed
    if unknown:
        raise ContractError(f"{label}: unknown keys {sorted(unknown)}")


def sanitize_type(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or not isinstance(value.get("kind"), str):
        raise ContractError(f"{label}: malformed type")
    kind = value["kind"]
    allowed = TYPE_KEYS.get(kind)
    if allowed is None:
        raise ContractError(f"{label}: unknown type kind {kind}")
    checked_keys(value, allowed, label)
    result: dict[str, Any] = {"kind": kind}
    if kind == "HttpTypeObject":
        members = value.get("members")
        if (
            not isinstance(members, list)
            or any(set(item) != {"ident"} for item in members if isinstance(item, dict))
            or any(not isinstance(item, dict) for item in members)
        ):
            raise ContractError(f"{label}: malformed object members")
        result["members"] = [item["ident"] for item in members]
    elif kind == "HttpTypeUnion":
        result["oas_ref"] = value.get("oasRef")
        result["types"] = [
            sanitize_type(item, f"{label} union") for item in value.get("types", [])
        ]
    elif kind == "HttpTypeLiteral":
        result["literal"] = value.get("literal")
    elif kind == "HttpTypeArray":
        result["oas_ref"] = value.get("oasRef")
        result["element_type"] = sanitize_type(
            value.get("elementType"), f"{label} element"
        )
    elif kind == "HttpTypeReference":
        result.update(
            {
                "ident": value.get("ident"),
                "ref": value.get("$ref"),
                "oas_ref": value.get("oasRef"),
            }
        )
        result["type_parameters"] = [
            sanitize_type(item, f"{label} parameter")
            for item in value.get("typeParameters", [])
        ]
    return result


def sanitize_node(value: Any, source_key: str, graph: dict[str, Any]) -> dict[str, Any]:
    if not isinstance(value, dict) or value.get("kind") not in NODE_KEYS:
        raise ContractError(f"{source_key}: unknown declaration")
    kind = value["kind"]
    checked_keys(value, NODE_KEYS[kind], source_key)
    children = value.get("children", [])
    if not isinstance(children, list) or not all(
        isinstance(item, str) for item in children
    ):
        raise ContractError(f"{source_key}: malformed children")
    missing = [item for item in children if item not in graph]
    if missing:
        raise ContractError(f"{source_key}: missing child {missing[0]}")
    if kind == "HttpDeclTypeAlias":
        return {
            "kind": kind,
            "ident": value["ident"],
            "oas_ref": value["oasRef"],
            "type": sanitize_type(value["type"], source_key),
            "children_parent_schema": value.get("childrenParentSchema"),
            "children": children,
        }
    if kind == "HttpDeclReference":
        return {
            "kind": kind,
            "type": sanitize_type(value["type"], source_key),
            "children_parent_schema": value.get("childrenParentSchema"),
            "children": children,
        }
    constraints = value.get("constraints", {})
    if not isinstance(constraints, dict) or set(constraints) - {
        "format",
        "maxLength",
        "maximum",
        "minLength",
        "minimum",
    }:
        raise ContractError(f"{source_key}: unknown constraints")
    return {
        "kind": kind,
        "key": value["key"],
        "optional": value["optional"],
        "nullable": value["nullable"],
        "deprecated": value["deprecated"],
        "oas_ref": value["oasRef"],
        "schema_type": value["schemaType"],
        "type": sanitize_type(value["type"], source_key),
        "children_parent_schema": value.get("childrenParentSchema"),
        "children": children,
        "constraints": constraints,
        "default_present": "default" in value,
        "default": value.get("default"),
    }


def sanitized_graph(
    kind: str, schema: str, root_key: str, graph: dict[str, Any]
) -> dict[str, Any]:
    nodes = [
        {"source_key": key, "declaration": sanitize_node(graph[key], key, graph)}
        for key in sorted(graph)
    ]
    return {
        "schema_version": SCHEMA_VERSION,
        "kind": kind,
        "schema": schema,
        "root_source_key": root_key,
        "nodes": nodes,
    }


def root_by_oas(
    graph: dict[str, Any], oas_ref: str, label: str
) -> tuple[str, dict[str, Any]]:
    matches = [
        (key, value)
        for key, value in graph.items()
        if isinstance(value, dict)
        and value.get("kind") == "HttpDeclTypeAlias"
        and value.get("oasRef") == oas_ref
    ]
    if len(matches) != 1:
        raise ContractError(f"{label}: expected one root, found {len(matches)}")
    return matches[0]


def direct_properties(
    graph: dict[str, Any], node: dict[str, Any], label: str
) -> list[dict[str, Any]]:
    properties = [
        graph[key]
        for key in node.get("children", [])
        if graph[key].get("kind") == "HttpDeclProperty"
    ]
    if not properties:
        raise ContractError(f"{label}: no properties")
    return properties


def literal_values(value: Any) -> list[Any]:
    if not isinstance(value, dict):
        return []
    found = [value["literal"]] if value.get("kind") == "HttpTypeLiteral" else []
    for child in value.values():
        if isinstance(child, list):
            for item in child:
                found.extend(literal_values(item))
        elif isinstance(child, dict):
            found.extend(literal_values(child))
    return found


def event_record(name: str, schema: str, graph: dict[str, Any]) -> dict[str, Any]:
    _, root = root_by_oas(graph, f"#/components/schemas/{schema}", name)
    properties = direct_properties(graph, root, name)
    records = {}
    for prop in properties:
        records[prop["key"]] = {
            "required": not prop["optional"],
            "nullable": prop["nullable"],
            "deprecated": prop["deprecated"],
            "oas_ref": prop["oasRef"],
            "schema_type": prop["schemaType"],
            "type": sanitize_type(prop["type"], f"{name}.{prop['key']}"),
        }
    type_literals = literal_values(
        next(prop["type"] for prop in properties if prop["key"] == "type")
    )
    sequence = records.get("sequence_number")
    if (
        type_literals != [name]
        or sequence is None
        or not sequence["required"]
        or sequence["nullable"]
    ):
        raise ContractError(f"{name}: invalid discriminator or sequence_number")
    return {
        "event": name,
        "schema": schema,
        "oas_ref": f"#/components/schemas/{schema}",
        "required": sorted(key for key, item in records.items() if item["required"]),
        "optional": sorted(
            key for key, item in records.items() if not item["required"]
        ),
        "nullable": sorted(key for key, item in records.items() if item["nullable"]),
        "properties": dict(sorted(records.items())),
    }


def variant_record(graph: dict[str, Any], key: str, index: int) -> dict[str, Any]:
    node = graph[key]
    properties = direct_properties(graph, node, key)
    type_prop = next((item for item in properties if item["key"] == "type"), None)
    if type_prop is None:
        raise ContractError(f"{key}: discriminator is missing")
    ident = node.get("ident") or node.get("type", {}).get("ident")
    return {
        "index": index,
        "schema": ident,
        "accepted_literals": literal_values(type_prop["type"]),
    }


def variants(graph: dict[str, Any], keys: list[str]) -> list[dict[str, Any]]:
    return [variant_record(graph, key, index) for index, key in enumerate(keys)]


def node_by_oas(
    graph: dict[str, Any], kind: str, oas_ref: str
) -> tuple[str, dict[str, Any]]:
    matches = [
        (key, value)
        for key, value in graph.items()
        if value.get("kind") == kind and value.get("oasRef") == oas_ref
    ]
    if not matches:
        raise ContractError(f"missing {kind} {oas_ref}")
    return matches[0]


def shape_record(graph: dict[str, Any], key: str) -> dict[str, Any]:
    node = graph[key]
    props = direct_properties(graph, node, key)
    return {
        "schema": node.get("ident") or node.get("type", {}).get("ident"),
        "required": sorted(item["key"] for item in props if not item["optional"]),
        "optional": sorted(item["key"] for item in props if item["optional"]),
        "nullable": sorted(item["key"] for item in props if item["nullable"]),
    }


def require_equal(actual: Any, expected: Any, label: str) -> None:
    if actual != expected:
        raise ContractError(f"{label}: expected {expected!r}, found {actual!r}")


def openapi_contract(value: Any, path: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ContractError(f"{path}: OpenAPI document must be an object")
    post = value["paths"][path]["post"]
    content = post["responses"]["200"]["content"]
    return {
        "path": path,
        "method": "post",
        "operation_id": post["operationId"],
        "request_schema_ref": post["requestBody"]["content"]["application/json"][
            "schema"
        ]["$ref"],
        "json_response_schema_ref": content["application/json"]["schema"]["$ref"],
        "stream_response_schema_ref": content.get("text/event-stream", {})
        .get("schema", {})
        .get("$ref"),
        "components_in_endpoint_slice": "components" in value,
    }
