#!/usr/bin/env python3
"""Validate one strict JSON document against the P1 evidence-schema dialect."""

import json
import re
import sys
from pathlib import Path
from typing import Any

ANNOTATION_KEYWORDS = {"$id", "$schema", "title"}
ASSERTION_KEYWORDS = {
    "$defs",
    "$ref",
    "additionalProperties",
    "allOf",
    "const",
    "else",
    "enum",
    "if",
    "items",
    "maxItems",
    "minItems",
    "minimum",
    "minLength",
    "pattern",
    "properties",
    "required",
    "then",
    "type",
    "uniqueItems",
}
JSON_TYPES = {"array", "boolean", "integer", "null", "object", "string"}


class ValidationError(Exception):
    """A strict JSON or supported-schema validation failure."""


def strict_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, member in pairs:
        if key in value:
            raise ValidationError(f"duplicate JSON key: {key}")
        value[key] = member
    return value


def reject_constant(value: str) -> None:
    raise ValidationError(f"non-standard JSON constant: {value}")


def load_json(path: Path) -> Any:
    try:
        return json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=strict_object,
            parse_constant=reject_constant,
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ValidationError(
            f"cannot read strict JSON: {type(error).__name__}"
        ) from error


def validate_schema(schema: Any, location: str = "schema") -> None:
    if not isinstance(schema, dict):
        raise ValidationError(f"{location} must be an object")
    unknown = set(schema) - ANNOTATION_KEYWORDS - ASSERTION_KEYWORDS
    if unknown:
        raise ValidationError(f"{location} uses an unsupported keyword")
    for key in ANNOTATION_KEYWORDS:
        if key in schema and not isinstance(schema[key], str):
            raise ValidationError(f"{location}.{key} must be a string")
    validate_type_keyword(schema.get("type"), location)
    validate_string_array(schema.get("required"), f"{location}.required")
    validate_non_negative_integer(schema.get("minItems"), f"{location}.minItems")
    validate_non_negative_integer(schema.get("maxItems"), f"{location}.maxItems")
    validate_non_negative_integer(schema.get("minLength"), f"{location}.minLength")
    validate_integer(schema.get("minimum"), f"{location}.minimum")
    for key in ("additionalProperties", "uniqueItems"):
        if key in schema and not isinstance(schema[key], bool):
            raise ValidationError(f"{location}.{key} must be boolean")
    if "pattern" in schema:
        if not isinstance(schema["pattern"], str):
            raise ValidationError(f"{location}.pattern must be a string")
        try:
            re.compile(schema["pattern"])
        except re.error as error:
            raise ValidationError(f"{location}.pattern is invalid") from error
    if "$ref" in schema and not isinstance(schema["$ref"], str):
        raise ValidationError(f"{location}.$ref must be a string")
    if "enum" in schema and (
        not isinstance(schema["enum"], list) or not schema["enum"]
    ):
        raise ValidationError(f"{location}.enum must be a non-empty array")
    for key in ("$defs", "properties"):
        members = schema.get(key)
        if members is None:
            continue
        if not isinstance(members, dict) or not all(
            isinstance(name, str) for name in members
        ):
            raise ValidationError(f"{location}.{key} must be an object")
        for name, member in members.items():
            validate_schema(member, f"{location}.{key}.{name}")
    if "items" in schema:
        validate_schema(schema["items"], f"{location}.items")
    for key in ("if", "then", "else"):
        if key in schema:
            validate_schema(schema[key], f"{location}.{key}")
    if "allOf" in schema:
        members = schema["allOf"]
        if not isinstance(members, list) or not members:
            raise ValidationError(f"{location}.allOf must be a non-empty array")
        for index, member in enumerate(members):
            validate_schema(member, f"{location}.allOf[{index}]")


def validate_type_keyword(value: Any, location: str) -> None:
    if value is None:
        return
    if isinstance(value, str):
        values = [value]
    elif isinstance(value, list) and value:
        values = value
    else:
        raise ValidationError(f"{location}.type is invalid")
    if not all(isinstance(member, str) and member in JSON_TYPES for member in values):
        raise ValidationError(f"{location}.type is invalid")
    if len(set(values)) != len(values):
        raise ValidationError(f"{location}.type contains duplicates")


def validate_string_array(value: Any, location: str) -> None:
    if value is None:
        return
    if not isinstance(value, list) or not all(
        isinstance(member, str) for member in value
    ):
        raise ValidationError(f"{location} must be an array of strings")
    if len(set(value)) != len(value):
        raise ValidationError(f"{location} contains duplicates")


def validate_non_negative_integer(value: Any, location: str) -> None:
    if value is not None and (
        isinstance(value, bool) or not isinstance(value, int) or value < 0
    ):
        raise ValidationError(f"{location} must be a non-negative integer")


def validate_integer(value: Any, location: str) -> None:
    if value is not None and (isinstance(value, bool) or not isinstance(value, int)):
        raise ValidationError(f"{location} must be an integer")


def validate_instance(
    instance: Any, schema: dict[str, Any], root: dict[str, Any], location: str = "$"
) -> None:
    if "$ref" in schema:
        validate_instance(
            instance, resolve_reference(root, schema["$ref"]), root, location
        )
    for member in schema.get("allOf", []):
        validate_instance(instance, member, root, location)
    condition = schema.get("if")
    if condition is not None:
        branch = (
            schema.get("then")
            if matches(instance, condition, root)
            else schema.get("else")
        )
        if branch is not None:
            validate_instance(instance, branch, root, location)
    if "type" in schema and not matches_type(instance, schema["type"]):
        raise ValidationError(f"{location} has the wrong JSON type")
    if "const" in schema and not json_equal(instance, schema["const"]):
        raise ValidationError(f"{location} does not match const")
    if "enum" in schema and not any(
        json_equal(instance, member) for member in schema["enum"]
    ):
        raise ValidationError(f"{location} is outside enum")
    if isinstance(instance, dict):
        validate_object(instance, schema, root, location)
    if isinstance(instance, list):
        validate_array(instance, schema, root, location)
    if isinstance(instance, str):
        validate_string(instance, schema, location)
    if isinstance(instance, int) and not isinstance(instance, bool):
        minimum = schema.get("minimum")
        if minimum is not None and instance < minimum:
            raise ValidationError(f"{location} is below minimum")


def resolve_reference(root: dict[str, Any], reference: str) -> dict[str, Any]:
    if not reference.startswith("#/"):
        raise ValidationError("only local JSON pointers are supported")
    value: Any = root
    for encoded in reference[2:].split("/"):
        token = encoded.replace("~1", "/").replace("~0", "~")
        if not isinstance(value, dict) or token not in value:
            raise ValidationError("JSON schema reference is unresolved")
        value = value[token]
    if not isinstance(value, dict):
        raise ValidationError("JSON schema reference does not name a schema")
    return value


def matches(instance: Any, schema: dict[str, Any], root: dict[str, Any]) -> bool:
    try:
        validate_instance(instance, schema, root)
    except ValidationError:
        return False
    return True


def matches_type(instance: Any, declared: str | list[str]) -> bool:
    types = [declared] if isinstance(declared, str) else declared
    return any(
        (name == "null" and instance is None)
        or (name == "boolean" and isinstance(instance, bool))
        or (
            name == "integer"
            and isinstance(instance, int)
            and not isinstance(instance, bool)
        )
        or (name == "string" and isinstance(instance, str))
        or (name == "array" and isinstance(instance, list))
        or (name == "object" and isinstance(instance, dict))
        for name in types
    )


def json_equal(left: Any, right: Any) -> bool:
    if type(left) is not type(right):
        return False
    if isinstance(left, dict):
        return left.keys() == right.keys() and all(
            json_equal(left[key], right[key]) for key in left
        )
    if isinstance(left, list):
        return len(left) == len(right) and all(
            json_equal(a, b) for a, b in zip(left, right, strict=True)
        )
    return left == right


def validate_object(
    instance: dict[str, Any],
    schema: dict[str, Any],
    root: dict[str, Any],
    location: str,
) -> None:
    required = schema.get("required", [])
    missing = [name for name in required if name not in instance]
    if missing:
        raise ValidationError(f"{location} is missing required properties")
    properties = schema.get("properties", {})
    if schema.get("additionalProperties") is False and set(instance) - set(properties):
        raise ValidationError(f"{location} has additional properties")
    for name, member_schema in properties.items():
        if name in instance:
            validate_instance(instance[name], member_schema, root, f"{location}.{name}")


def validate_array(
    instance: list[Any], schema: dict[str, Any], root: dict[str, Any], location: str
) -> None:
    minimum = schema.get("minItems")
    maximum = schema.get("maxItems")
    if minimum is not None and len(instance) < minimum:
        raise ValidationError(f"{location} has too few items")
    if maximum is not None and len(instance) > maximum:
        raise ValidationError(f"{location} has too many items")
    if schema.get("uniqueItems") and any(
        json_equal(left, right)
        for index, left in enumerate(instance)
        for right in instance[index + 1 :]
    ):
        raise ValidationError(f"{location} has duplicate items")
    item_schema = schema.get("items")
    if item_schema is not None:
        for index, member in enumerate(instance):
            validate_instance(member, item_schema, root, f"{location}[{index}]")


def validate_string(instance: str, schema: dict[str, Any], location: str) -> None:
    minimum = schema.get("minLength")
    if minimum is not None and len(instance) < minimum:
        raise ValidationError(f"{location} is too short")
    pattern = schema.get("pattern")
    if pattern is not None and re.search(pattern, instance) is None:
        raise ValidationError(f"{location} does not match pattern")


def main() -> int:
    if len(sys.argv) != 3:
        raise ValidationError("validator requires schema and instance paths")
    schema = load_json(Path(sys.argv[1]))
    instance = load_json(Path(sys.argv[2]))
    validate_schema(schema)
    validate_instance(instance, schema, schema)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValidationError as error:
        print(f"P1 evidence validation failed: {error}", file=sys.stderr)
        raise SystemExit(1) from None
