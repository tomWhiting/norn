#!/usr/bin/env python3
"""Executable generator and semantic-coverage checks for the P1 corpus."""

from __future__ import annotations

import json
import re
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import responses_fixture_generate as generator
from responses_fixture_types import APPROVED_SOURCE_REFERENCES

SYNTHETIC = re.compile(r"^norn-synthetic-[a-z0-9][a-z0-9-]*$")

REQUIRED_MARKERS = {
    "ordered-output-items",
    "assistant-phase",
    "encrypted-reasoning",
    "phase-absent",
    "phase-null",
    "phase-commentary",
    "phase-final-answer",
    "function-call",
    "custom-call",
    "typed-refusal",
    "hosted-web-search",
    "url-annotation",
    "compaction",
    "usage-absent",
    "usage-null",
    "usage-reported-zero",
    "cache-write-present-positive",
    "standalone-error",
    "retry-after-over-ceiling",
    "failed-incomplete-successful-attempts",
    "interleaved-identities",
    "duplicate-completion",
    "malformed-terminal",
    "unknown-event",
    "unknown-actionable-item",
    "codex-end-turn-false",
    "codex-end-turn-true",
    "turn-state-header-receipt",
    "metadata-event-receipt",
    "within-turn-replay",
    "turn-boundary-clear",
}

FIXED_LITERALS = {
    "24h",
    "30m",
    "assistant",
    "auto",
    "cancelled",
    "commentary",
    "compaction",
    "compaction_trigger",
    "completed",
    "content_filter",
    "developer",
    "disabled",
    "error",
    "explicit",
    "failed",
    "final_answer",
    "function",
    "function_call",
    "function_call_output",
    "high",
    "implicit",
    "in_memory",
    "in_progress",
    "incomplete",
    "input_text",
    "json_schema",
    "max_output_tokens",
    "none",
    "message",
    "object",
    "output_text",
    "public",
    "queued",
    "required",
    "reasoning",
    "refusal",
    "response",
    "response.metadata",
    "system",
    "summary_text",
    "string",
    "custom",
    "custom_tool_call",
    "apply_patch",
    "search",
    "url",
    "url_citation",
    "web_search",
    "user",
}


def strict_json(value: bytes) -> object:
    def reject_duplicates(pairs: list[tuple[str, object]]) -> dict[str, object]:
        result: dict[str, object] = {}
        for key, item in pairs:
            if key in result:
                raise ValueError("duplicate JSON member")
            result[key] = item
        return result

    return json.loads(value, object_pairs_hook=reject_duplicates)


def fixture_documents(files: dict[str, bytes]) -> list[tuple[str, dict[str, object]]]:
    documents = []
    for path, value in files.items():
        if "/requests/" in path or "/transport/" in path:
            document = strict_json(value)
            if not isinstance(document, dict):
                raise ValueError("fixture is not a JSON object")
            documents.append((path, document))
    return documents


def stream_events(value: bytes) -> tuple[dict[str, object], list[dict[str, object]]]:
    lines = value.decode().splitlines()
    prefix = ": norn-fixture-v1 "
    if not lines or not lines[0].startswith(prefix):
        raise ValueError("stream fixture has no metadata envelope")
    metadata = strict_json(lines[0][len(prefix) :].encode())
    if not isinstance(metadata, dict):
        raise ValueError("stream metadata is not an object")
    events = []
    event_name = None
    for line in lines[1:]:
        if line.startswith("event: "):
            event_name = line.removeprefix("event: ")
        elif line.startswith("data: "):
            event = strict_json(line.removeprefix("data: ").encode())
            if not isinstance(event, dict) or event.get("type") != event_name:
                raise ValueError("SSE event name does not match data type")
            events.append(event)
            event_name = None
        elif line:
            raise ValueError("unexpected SSE line")
    if not events or event_name is not None:
        raise ValueError("stream fixture is empty or incomplete")
    return metadata, events


class FixtureCorpusTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.rows = generator.load_traceability()
        cls.specs = generator.all_specs()
        cls.files = generator.build_corpus()

    def test_exact_planned_inventory_is_generated(self) -> None:
        planned = {
            row["planned_fixture_ids"][0]
            for row in self.rows.values()
            if row["fixture_applicability"] == "planned"
        }
        self.assertEqual(len(planned), 39)
        self.assertEqual({generator.fixture_identity(spec, self.rows[spec.finding_id]) for spec in self.specs}, planned)
        self.assertEqual(len(self.files), 44)

    def test_catalog_covers_every_ratified_semantic_category(self) -> None:
        markers = {marker for spec in self.specs for marker in spec.semantic_markers}
        self.assertTrue(REQUIRED_MARKERS.issubset(markers), REQUIRED_MARKERS - markers)

    def test_manifests_derive_finding_and_owner_from_traceability(self) -> None:
        for dialect in ("public", "codex"):
            path = f"crates/norn/testdata/openai_responses/{dialect}/manifest.json"
            document = strict_json(self.files[path])
            fixtures = document["payload"]["fixtures"]
            self.assertEqual(fixtures, sorted(fixtures, key=lambda item: item["id"]))
            for item in fixtures:
                finding_id = item["finding_ids"][0]
                row = self.rows[finding_id]
                self.assertEqual(item["id"], row["planned_fixture_ids"][0])
                self.assertEqual(item["owner_phase"], row["owner_phase"])
                self.assertEqual(item["expectation_class"], row["expectation_class"])
                self.assertNotIn("owner", item)

    def test_manifest_hashes_bind_every_fixture(self) -> None:
        registered_paths = set()
        for dialect in ("public", "codex"):
            path = f"crates/norn/testdata/openai_responses/{dialect}/manifest.json"
            fixtures = strict_json(self.files[path])["payload"]["fixtures"]
            for item in fixtures:
                value = self.files[item["fixture_path"]]
                self.assertEqual(item["bytes"], len(value))
                self.assertEqual(item["sha256"], generator.digest_bytes(value))
                registered_paths.add(item["fixture_path"])
        concrete = {
            path
            for path in self.files
            if "/requests/" in path or "/streams/" in path or "/transport/" in path
        }
        self.assertEqual(registered_paths, concrete)

    def test_json_and_sse_envelopes_match_manifest_identity(self) -> None:
        for path, document in fixture_documents(self.files):
            self.assertEqual(document["schema_version"], 1)
            self.assertEqual(document["artifact_family"], "protocol_fixture")
            self.assertIsInstance(document["payload"], dict)
            self.assertIn(f"/{document['dialect']}/", path)
        for path, value in self.files.items():
            if "/streams/" not in path:
                continue
            metadata, events = stream_events(value)
            self.assertEqual(metadata["artifact_kind"], "stream")
            self.assertIn(f"/{metadata['dialect']}/", path)
            sequences = [event["sequence_number"] for event in events]
            self.assertEqual(sequences, list(range(1, len(events) + 1)))

    def test_payload_free_form_strings_are_registered_style_or_official(self) -> None:
        public_literals = set(FIXED_LITERALS)
        inventories = strict_json(
            (generator.REPOSITORY_ROOT / "policy/contracts/openai-responses-v1/inventories.json").read_bytes()
        )
        _collect_official_literals(inventories, public_literals)
        sse = strict_json(
            (generator.REPOSITORY_ROOT / "policy/contracts/openai-responses-v1/sse-events.json").read_bytes()
        )
        _collect_official_literals(sse, public_literals)
        for _, document in fixture_documents(self.files):
            _assert_safe_strings(self, document["payload"], public_literals)
        for path, value in self.files.items():
            if "/streams/" in path:
                _, events = stream_events(value)
                _assert_safe_strings(self, events, public_literals)

    def test_sources_are_exact_official_openai_or_pinned_codex_urls(self) -> None:
        for spec in self.specs:
            for source in spec.source_references:
                self.assertIn(source, APPROVED_SOURCE_REFERENCES)

    def test_checked_in_corpus_is_current(self) -> None:
        generator.check_corpus(self.files)


def _collect_official_literals(value: object, output: set[str]) -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            if key in {"accepted_literals", "include_values", "incomplete_reasons", "response_statuses"}:
                if isinstance(child, list):
                    output.update(item for item in child if isinstance(item, str))
            elif key == "cache_controls" and isinstance(child, dict):
                for literals in child.values():
                    if isinstance(literals, list):
                        output.update(item for item in literals if isinstance(item, str))
            elif key == "event" and isinstance(child, str):
                output.add(child)
            _collect_official_literals(child, output)
    elif isinstance(value, list):
        for child in value:
            _collect_official_literals(child, output)


def _assert_safe_strings(
    case: unittest.TestCase, value: object, official_literals: set[str]
) -> None:
    if isinstance(value, str):
        accepted = (
            bool(SYNTHETIC.fullmatch(value))
            or value in official_literals
            or value.startswith("https://developers.openai.com/")
            or value.startswith("https://api.openai.com/")
            or value.startswith("#/")
        )
        case.assertTrue(accepted, value)
    elif isinstance(value, dict):
        for child in value.values():
            _assert_safe_strings(case, child, official_literals)
    elif isinstance(value, list):
        for child in value:
            _assert_safe_strings(case, child, official_literals)


if __name__ == "__main__":
    unittest.main()
