"""Self-tests for the pinned public Responses contract extractor."""

from __future__ import annotations

import hashlib
import importlib.util
import re
import sys
import unittest
from pathlib import Path
from typing import Any


sys.dont_write_bytecode = True


ROOT = Path(__file__).resolve().parents[4]
CONTRACT = ROOT / "policy/contracts/openai-responses-v1"
MODULE_PATH = Path(__file__).with_name("openai_contract_extract.py")
SPEC = importlib.util.spec_from_file_location("openai_contract_extract", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("cannot load extractor")
extractor = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(extractor)


def load(path: Path) -> Any:
    return extractor.parse_json(path.read_text(), str(path))


def type_matches(value: Any, expected: str) -> bool:
    checks = {
        "array": lambda item: isinstance(item, list),
        "boolean": lambda item: type(item) is bool,
        "integer": lambda item: type(item) is int,
        "null": lambda item: item is None,
        "number": lambda item: type(item) in (int, float),
        "object": lambda item: isinstance(item, dict),
        "string": lambda item: isinstance(item, str),
    }
    return checks[expected](value)


def resolve_ref(root: dict[str, Any], ref: str) -> dict[str, Any]:
    if not ref.startswith("#/"):
        raise AssertionError(f"external schema reference: {ref}")
    value: Any = root
    for component in ref[2:].split("/"):
        value = value[component.replace("~1", "/").replace("~0", "~")]
    return value


def schema_errors(
    value: Any, schema: dict[str, Any], root: dict[str, Any], path: str = "$"
) -> list[str]:
    if "$ref" in schema:
        return schema_errors(value, resolve_ref(root, schema["$ref"]), root, path)
    if "oneOf" in schema:
        attempts = [schema_errors(value, item, root, path) for item in schema["oneOf"]]
        matches = sum(not errors for errors in attempts)
        return [] if matches == 1 else [f"{path}: oneOf matched {matches} branches"]
    errors: list[str] = []
    expected_type = schema.get("type")
    if expected_type is not None:
        accepted = expected_type if isinstance(expected_type, list) else [expected_type]
        if not any(type_matches(value, item) for item in accepted):
            return [f"{path}: wrong type"]
    if "const" in schema and value != schema["const"]:
        errors.append(f"{path}: wrong constant")
    if "enum" in schema and value not in schema["enum"]:
        errors.append(f"{path}: value outside enum")
    if (
        isinstance(value, str)
        and "pattern" in schema
        and re.search(schema["pattern"], value) is None
    ):
        errors.append(f"{path}: pattern mismatch")
    if (
        type(value) in (int, float)
        and "minimum" in schema
        and value < schema["minimum"]
    ):
        errors.append(f"{path}: below minimum")
    if isinstance(value, list):
        if len(value) < schema.get("minItems", 0):
            errors.append(f"{path}: too few items")
        if "maxItems" in schema and len(value) > schema["maxItems"]:
            errors.append(f"{path}: too many items")
        if "items" in schema:
            for index, item in enumerate(value):
                errors.extend(
                    schema_errors(item, schema["items"], root, f"{path}[{index}]")
                )
    if isinstance(value, dict):
        properties = schema.get("properties", {})
        missing = set(schema.get("required", [])) - set(value)
        if missing:
            errors.append(f"{path}: missing {sorted(missing)}")
        for key, item in value.items():
            if key in properties:
                errors.extend(
                    schema_errors(item, properties[key], root, f"{path}.{key}")
                )
            elif schema.get("additionalProperties") is False:
                errors.append(f"{path}: unexpected {key}")
            elif isinstance(schema.get("additionalProperties"), dict):
                errors.extend(
                    schema_errors(
                        item, schema["additionalProperties"], root, f"{path}.{key}"
                    )
                )
    return errors


class ExtractorUnitTests(unittest.TestCase):
    def test_duplicate_json_keys_are_rejected(self) -> None:
        with self.assertRaises(extractor.ContractError):
            extractor.parse_json('{"same": 1, "same": 2}', "duplicate")

    def test_unknown_source_fields_and_type_kinds_fail(self) -> None:
        graph = {
            "root": {
                "kind": "HttpDeclTypeAlias",
                "ident": "Root",
                "oasRef": "#/Root",
                "type": {"kind": "HttpTypeObject", "members": []},
                "children": [],
                "unexpected": True,
            }
        }
        with self.assertRaises(extractor.ContractError):
            extractor.sanitize_node(graph["root"], "root", graph)
        with self.assertRaises(extractor.ContractError):
            extractor.sanitize_type({"kind": "NewGeneratorType"}, "new type")

    def test_schema_fence_tolerates_nested_markdown_fence(self) -> None:
        section = """## event\nSchema name: `Example`\n```json\n{
  \"root\": {
    \"kind\": \"HttpDeclTypeAlias\",
    \"ident\": \"Example\",
    \"oasRef\": \"#/components/schemas/Example\",
    \"type\": {\"kind\": \"HttpTypeObject\", \"members\": []},
    \"docstring\": \"nested\\n```json\\n{}\\n```\",
    \"children\": []
  }
}
```\n### Example\n```json\n{}\n```\n"""
        name, graph = extractor.schema_graph(section, "\n### Example", "nested")
        self.assertEqual(name, "Example")
        self.assertIn("root", graph)

    def test_root_selection_uses_oas_ref_not_generated_ident(self) -> None:
        graph = {
            "root": {
                "kind": "HttpDeclTypeAlias",
                "ident": "ResponseMcpCallEvent",
                "oasRef": "#/components/schemas/ResponseMCPCallEvent",
            }
        }
        key, _ = extractor.root_by_oas(
            graph, "#/components/schemas/ResponseMCPCallEvent", "MCP casing"
        )
        self.assertEqual(key, "root")

    def test_canonical_serialization_is_stable(self) -> None:
        value = {"z": [3, 2, 1], "a": {"b": True}}
        self.assertEqual(extractor.canonical(value), extractor.canonical(value))
        self.assertTrue(extractor.canonical(value).endswith(b"\n"))


class CheckedContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.schema = load(CONTRACT / "contract.schema.json")
        cls.artifacts = {
            path.name: load(path)
            for path in CONTRACT.glob("*.json")
            if path.name != "contract.schema.json"
        }

    def test_schema_is_closed_at_every_declared_object(self) -> None:
        def walk(value: Any, path: str) -> list[str]:
            errors = []
            if isinstance(value, dict):
                if (
                    value.get("type") == "object"
                    and "properties" in value
                    and "additionalProperties" not in value
                ):
                    errors.append(path)
                for key, item in value.items():
                    errors.extend(walk(item, f"{path}.{key}"))
            elif isinstance(value, list):
                for index, item in enumerate(value):
                    errors.extend(walk(item, f"{path}[{index}]"))
            return errors

        self.assertEqual(walk(self.schema, "$"), [])

    def test_every_artifact_satisfies_the_closed_schema(self) -> None:
        for name, artifact in self.artifacts.items():
            with self.subTest(name=name):
                self.assertEqual(schema_errors(artifact, self.schema, self.schema), [])

    def test_manifest_hashes_exact_outputs_without_self_hash(self) -> None:
        manifest = self.artifacts["manifest.json"]
        paths = [item["path"] for item in manifest["outputs"]]
        self.assertNotIn("policy/contracts/openai-responses-v1/manifest.json", paths)
        self.assertEqual(len(paths), len(set(paths)))
        for item in manifest["outputs"]:
            path = ROOT / item["path"]
            data = path.read_bytes()
            self.assertEqual(item["bytes"], len(data))
            self.assertEqual(item["sha256"], hashlib.sha256(data).hexdigest())

    def test_inventory_counts_and_literals_are_exact(self) -> None:
        inventory = self.artifacts["inventories.json"]
        self.assertEqual(len(inventory["input_variants"]), 32)
        self.assertEqual(len(inventory["output_variants"]), 28)
        self.assertEqual(len(inventory["tool_variants"]), 16)
        self.assertEqual(
            sum(len(item["accepted_literals"]) for item in inventory["tool_variants"]),
            18,
        )
        self.assertEqual(len(inventory["annotation_variants"]), 4)
        self.assertEqual(len(inventory["include_values"]), 8)
        self.assertEqual(len(inventory["response_statuses"]), 6)
        self.assertEqual(len(inventory["usage_paths"]), 6)

    def test_assistant_phase_remains_omittable_and_nullable(self) -> None:
        phase_nodes = []
        phase_refs = set(extractor.PHASE_REFS)
        for name in ("request-graph.json", "response-graph.json"):
            for node in self.artifacts[name]["nodes"]:
                declaration = node["declaration"]
                if (
                    declaration["kind"] == "HttpDeclProperty"
                    and declaration["oas_ref"] in phase_refs
                ):
                    phase_nodes.append(declaration)
        self.assertEqual(len(phase_nodes), 4)
        self.assertTrue(all(item["optional"] for item in phase_nodes))
        self.assertTrue(all(item["nullable"] for item in phase_nodes))

    def test_all_events_are_structurally_pinned(self) -> None:
        events = self.artifacts["sse-events.json"]["events"]
        self.assertEqual(len(events), 53)
        self.assertEqual(len({item["event"] for item in events}), 53)
        for event in events:
            self.assertIn("sequence_number", event["required"])
            self.assertNotIn("sequence_number", event["nullable"])
        by_name = {item["event"]: item for item in events}
        self.assertEqual(
            by_name["response.reasoning_summary_part.done"]["optional"], ["status"]
        )
        self.assertEqual(by_name["error"]["nullable"], ["code", "param"])
        for name in (
            "response.audio.delta",
            "response.audio.done",
            "response.audio.transcript.delta",
            "response.audio.transcript.done",
        ):
            self.assertNotIn("response_id", by_name[name]["properties"])

    def test_exact_ten_source_discrepancies_are_retained(self) -> None:
        artifact = self.artifacts["source-discrepancies.json"]
        self.assertEqual(artifact["count"], 10)
        self.assertEqual(len(artifact["items"]), 10)
        audio = [item for item in artifact["items"] if item["field"] == "response_id"]
        self.assertEqual(len(audio), 4)
        self.assertTrue(
            all(item["classification"] == "example_only_unclassified" for item in audio)
        )
        self.assertEqual(
            artifact["gate_corrections"],
            [
                {
                    "classification": "superseded_gate_a_source_claim",
                    "id": "gate-a-01",
                    "oas_refs": list(extractor.PHASE_REFS),
                    "observed_nullable": True,
                    "observed_optional": True,
                    "source_ids": ["streaming", "websocket"],
                    "subject": "assistant_message_phase",
                    "superseded_claim": "optional_non_nullable",
                }
            ],
        )

    def test_sanitized_graphs_exclude_prose_and_examples(self) -> None:
        forbidden = {"docstring", "examples", "modelImplicit", "modelPath", "title"}
        for name in ("request-graph.json", "response-graph.json"):
            nodes = self.artifacts[name]["nodes"]
            self.assertEqual(len(nodes), len({item["source_key"] for item in nodes}))
            for node in nodes:
                self.assertFalse(forbidden & set(node["declaration"]))


if __name__ == "__main__":
    unittest.main()
