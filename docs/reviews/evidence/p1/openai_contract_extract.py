#!/usr/bin/env python3
"""Extract the pinned, disclosure-safe public Responses contract from MCP captures."""

from __future__ import annotations

import argparse
import importlib.util
import sys
from pathlib import Path
from types import ModuleType
from typing import Any


def _load_local_module(module_name: str, file_name: str) -> ModuleType:
    path = Path(__file__).with_name(file_name).resolve(strict=True)
    loaded = sys.modules.get(module_name)
    if loaded is not None:
        loaded_file = getattr(loaded, "__file__", None)
        if loaded_file is None or Path(loaded_file).resolve() != path:
            raise ImportError(f"{module_name}: unexpected module origin")
        return loaded
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        raise ImportError(f"{module_name}: cannot create module spec")
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    try:
        spec.loader.exec_module(module)
    except Exception:
        if sys.modules.get(module_name) is module:
            del sys.modules[module_name]
        raise
    return module


_constants = _load_local_module(
    "openai_contract_constants", "openai_contract_constants.py"
)
_graph = _load_local_module("openai_contract_graph", "openai_contract_graph.py")
_artifact_builder = _load_local_module(
    "openai_contract_build", "openai_contract_build.py"
)

SCHEMA_VERSION = _constants.SCHEMA_VERSION
EXTRACTOR_VERSION = _constants.EXTRACTOR_VERSION
EVENT_COUNT = _constants.EVENT_COUNT
INPUT_TYPES = _constants.INPUT_TYPES
OUTPUT_TYPES = _constants.OUTPUT_TYPES
TOOL_TYPES = _constants.TOOL_TYPES
ANNOTATIONS = _constants.ANNOTATIONS
INCLUDES = _constants.INCLUDES
STATUSES = _constants.STATUSES
INCOMPLETE_REASONS = _constants.INCOMPLETE_REASONS
PHASE_REFS = _constants.PHASE_REFS
EXPECTED_DISCREPANCIES = _constants.EXPECTED_DISCREPANCIES
NODE_KEYS = _constants.NODE_KEYS
TYPE_KEYS = _constants.TYPE_KEYS
STRIPPED_NODE_KEYS = _constants.STRIPPED_NODE_KEYS

ContractError = _graph.ContractError
reject_duplicate_pairs = _graph.reject_duplicate_pairs
parse_json = _graph.parse_json
canonical = _graph.canonical
digest = _graph.digest
normalized_source = _graph.normalized_source
markdown_sections = _graph.markdown_sections
named_section = _graph.named_section
schema_graph = _graph.schema_graph
example_keys = _graph.example_keys
checked_keys = _graph.checked_keys
sanitize_type = _graph.sanitize_type
sanitize_node = _graph.sanitize_node
sanitized_graph = _graph.sanitized_graph
root_by_oas = _graph.root_by_oas
direct_properties = _graph.direct_properties
literal_values = _graph.literal_values
event_record = _graph.event_record
variant_record = _graph.variant_record
variants = _graph.variants
node_by_oas = _graph.node_by_oas
shape_record = _graph.shape_record
require_equal = _graph.require_equal
openapi_contract = _graph.openapi_contract
build = _artifact_builder.build


def manifest(
    args: argparse.Namespace,
    artifacts: dict[str, bytes],
    metadata: dict[str, Any],
    schema_bytes: bytes,
) -> dict[str, Any]:
    outputs = dict(artifacts)
    outputs["contract.schema.json"] = schema_bytes
    return {
        "schema_version": SCHEMA_VERSION,
        "kind": "public_responses_contract_manifest",
        "retrieved_on": args.retrieved_on,
        "extractor_version": EXTRACTOR_VERSION,
        **metadata,
        "outputs": [
            {
                "path": f"policy/contracts/openai-responses-v1/{name}",
                "bytes": len(data),
                "sha256": digest(data),
            }
            for name, data in sorted(outputs.items())
        ],
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    for name in ("create", "compact", "streaming", "websocket"):
        parser.add_argument(f"--{name}", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--retrieved-on", required=True)
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    output = Path(args.output)
    schema_path = output / "contract.schema.json"
    schema_bytes = schema_path.read_bytes()
    parse_json(schema_bytes.decode(), "contract schema")
    artifacts, metadata = build(args)
    artifacts["manifest.json"] = canonical(
        manifest(args, artifacts, metadata, schema_bytes)
    )
    failures = []
    for name, expected in sorted(artifacts.items()):
        path = output / name
        if args.check:
            if not path.is_file() or path.read_bytes() != expected:
                failures.append(name)
        else:
            path.write_bytes(expected)
    if failures:
        raise ContractError(f"checked artifacts differ: {', '.join(failures)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
