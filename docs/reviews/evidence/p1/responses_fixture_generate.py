#!/usr/bin/env python3
"""Build or verify the deterministic sanitized Responses fixture corpus."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any, Iterable

from responses_fixture_codex import fixture_specs as codex_specs
from responses_fixture_public_requests import fixture_specs as request_specs
from responses_fixture_public_streams import fixture_specs as stream_specs
from responses_fixture_types import APPROVED_SOURCE_REFERENCES, CODEX_COMMIT, FixtureSpec

REPOSITORY_ROOT = Path(__file__).resolve().parents[4]
FIXTURE_ROOT = REPOSITORY_ROOT / "crates/norn/testdata/openai_responses"
TRACEABILITY_PATH = REPOSITORY_ROOT / "docs/reviews/evidence/p1/finding-traceability.jsonl"
PUBLIC_CONTRACT_MANIFEST = REPOSITORY_ROOT / "policy/contracts/openai-responses-v1/manifest.json"
TRACEABILITY_SHA256 = "190246d8738a41eb0f3afff7657de71c0d88eeb1bc871cce63d59714b30aa162"
PUBLIC_MANIFEST_REPOSITORY_PATH = "policy/contracts/openai-responses-v1/manifest.json"

CODEX_SOURCES = (
    ("codex-rs/core/src/client.rs", "f5896595c6fe1ec1b477096e5a41548039f673c7"),
    (
        "codex-rs/codex-api/src/sse/responses.rs",
        "70f96cb855005d577c57fd768062d035cc919b12",
    ),
    ("codex-rs/codex-api/src/common.rs", "e4600e26aab62a8495248346cd78ab3cb52b7191"),
    ("codex-rs/protocol/src/models.rs", "91fd42a5558a3836343ffb94ffef3a7f4050b332"),
    ("codex-rs/login/src/server.rs", "804d05434e231049ffa63709728a5ed8b004e247"),
)

BACKEND_CONCERNS = (
    "request_authority",
    "stored_continuation",
    "stateless_continuation",
    "assistant_phase",
    "turn_state",
    "completion",
    "metadata",
    "compaction",
    "error_retry_semantics",
    "cache_reporting",
)


class CorpusError(Exception):
    """A deterministic corpus input or output did not match its contract."""


def canonical_json(value: Any) -> bytes:
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=True) + "\n").encode()


def compact_json(value: Any) -> str:
    return json.dumps(value, separators=(",", ":"), sort_keys=True, ensure_ascii=True)


def canonical_compact_json(value: Any) -> bytes:
    return (compact_json(value) + "\n").encode()


def digest_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def file_reference(repository_path: str, value: bytes) -> dict[str, Any]:
    return {
        "path": repository_path,
        "bytes": len(value),
        "sha256": digest_bytes(value),
    }


def envelope(
    fixture_id: str,
    dialect: str,
    artifact_kind: str,
    payload: dict[str, Any],
) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "artifact_family": "protocol_fixture",
        "fixture_id": fixture_id,
        "dialect": dialect,
        "artifact_kind": artifact_kind,
        "payload": payload,
    }


def load_traceability() -> dict[str, dict[str, Any]]:
    raw = TRACEABILITY_PATH.read_bytes()
    if digest_bytes(raw) != TRACEABILITY_SHA256 or not raw.endswith(b"\n"):
        raise CorpusError("traceability registry does not match the ratified pin")
    rows: dict[str, dict[str, Any]] = {}
    for line in raw.splitlines():
        row = json.loads(line)
        finding_id = row["finding_id"]
        if finding_id in rows:
            raise CorpusError("traceability registry contains a duplicate finding")
        rows[finding_id] = row
    if len(rows) != 62:
        raise CorpusError("traceability registry does not contain 62 findings")
    return rows


def all_specs() -> tuple[FixtureSpec, ...]:
    specs = tuple(request_specs() + stream_specs() + codex_specs())
    finding_ids = [spec.finding_id for spec in specs]
    if len(finding_ids) != len(set(finding_ids)):
        raise CorpusError("fixture catalog contains a duplicate finding")
    return specs


def validate_catalog(
    specs: Iterable[FixtureSpec], rows: dict[str, dict[str, Any]]
) -> None:
    planned = {
        finding_id
        for finding_id, row in rows.items()
        if row["fixture_applicability"] == "planned"
    }
    actual = {spec.finding_id for spec in specs}
    if actual != planned:
        raise CorpusError("fixture catalog does not exactly cover planned findings")
    for spec in specs:
        row = rows[spec.finding_id]
        fixture_ids = row["planned_fixture_ids"]
        if len(fixture_ids) != 1 or not fixture_ids[0].startswith("fixture-"):
            raise CorpusError("planned fixture identity is not singular and stable")
        if row["owner_phase"] == "P0" or row["closure_status"] != "open":
            raise CorpusError("planned fixture points at a closed finding")
        if not spec.semantic_markers:
            raise CorpusError("fixture has no executable semantic markers")
        if len(spec.source_references) != len(set(spec.source_references)):
            raise CorpusError("fixture has duplicate source references")
        if any(source not in APPROVED_SOURCE_REFERENCES for source in spec.source_references):
            raise CorpusError("fixture source is not an exact approved authority")


def fixture_identity(spec: FixtureSpec, row: dict[str, Any]) -> str:
    fixture_ids = row["planned_fixture_ids"]
    if len(fixture_ids) != 1:
        raise CorpusError("fixture row does not have exactly one planned identity")
    return fixture_ids[0]


def fixture_repository_path(spec: FixtureSpec, fixture_id: str) -> str:
    stem = fixture_id.removeprefix("fixture-")
    if spec.artifact_kind == "stream":
        directory, extension = "streams", "sse"
    elif spec.artifact_kind == "transport":
        directory, extension = "transport", "json"
    else:
        directory, extension = "requests", "json"
    return f"crates/norn/testdata/openai_responses/{spec.dialect}/{directory}/{stem}.{extension}"


def render_fixture(spec: FixtureSpec, fixture_id: str) -> bytes:
    if spec.artifact_kind != "stream":
        if spec.payload is None:
            raise CorpusError("JSON fixture has no payload")
        return canonical_json(
            envelope(fixture_id, spec.dialect, spec.artifact_kind, spec.payload)
        )
    metadata = {
        "schema_version": 1,
        "artifact_family": "protocol_fixture",
        "fixture_id": fixture_id,
        "dialect": spec.dialect,
        "artifact_kind": "stream",
    }
    lines = [f": norn-fixture-v1 {compact_json(metadata)}"]
    for item in spec.events:
        event_type = item.get("type")
        if not isinstance(event_type, str):
            raise CorpusError("stream event has no string type")
        lines.extend((f"event: {event_type}", f"data: {compact_json(item)}", ""))
    return ("\n".join(lines) + "\n").encode()


def fixture_registration(
    spec: FixtureSpec,
    row: dict[str, Any],
    fixture_id: str,
    repository_path: str,
    value: bytes,
) -> dict[str, Any]:
    categories = [row["fixture_category"], "synthetic/contract"]
    if "synthetic-robustness" in spec.semantic_markers:
        categories.append("synthetic/robustness")
    finding_slug = spec.finding_id.lower().replace("_", "-")
    return {
        "id": fixture_id,
        "dialect": spec.dialect,
        "artifact_kind": spec.artifact_kind,
        "fixture_path": repository_path,
        "bytes": len(value),
        "sha256": digest_bytes(value),
        "source_references": list(spec.source_references),
        "categories": categories,
        "finding_ids": [spec.finding_id],
        "owner_phase": row["owner_phase"],
        "expectation_class": row["expectation_class"],
        "current_observation": (
            f"norn-synthetic-observation-no-provider-capture-{finding_slug}"
        ),
        "target_assertions": [f"norn-synthetic-contract-target-{finding_slug}"],
        "secret_profile": "registered_synthetic",
    }


def build_corpus() -> dict[str, bytes]:
    rows = load_traceability()
    specs = all_specs()
    validate_catalog(specs, rows)
    files: dict[str, bytes] = {}
    registrations: dict[str, list[dict[str, Any]]] = {"public": [], "codex": []}
    for spec in specs:
        row = rows[spec.finding_id]
        fixture_id = fixture_identity(spec, row)
        path = fixture_repository_path(spec, fixture_id)
        value = render_fixture(spec, fixture_id)
        if path in files:
            raise CorpusError("fixture catalog contains a duplicate path")
        files[path] = value
        registrations[spec.dialect].append(
            fixture_registration(spec, row, fixture_id, path, value)
        )
    for dialect in registrations:
        registrations[dialect].sort(key=lambda item: item["id"])

    public_manifest_path = "crates/norn/testdata/openai_responses/public/manifest.json"
    codex_manifest_path = "crates/norn/testdata/openai_responses/codex/manifest.json"
    public_manifest = canonical_compact_json(
        envelope(
            "openai-responses-public-manifest-v1",
            "public",
            "manifest",
            {"fixtures": registrations["public"]},
        )
    )
    codex_manifest = canonical_compact_json(
        envelope(
            "openai-responses-codex-manifest-v1",
            "codex",
            "manifest",
            {"fixtures": registrations["codex"]},
        )
    )
    files[public_manifest_path] = public_manifest
    files[codex_manifest_path] = codex_manifest
    files["crates/norn/testdata/openai_responses/contract-pins.json"] = _contract_pins()
    files["crates/norn/testdata/openai_responses/backend-state-matrix.json"] = _backend_matrix()
    files["crates/norn/testdata/openai_responses/index.json"] = canonical_json(
        envelope(
            "openai-responses-index-v1",
            "corpus",
            "index",
            {
                "public_manifest": file_reference(public_manifest_path, public_manifest),
                "codex_manifest": file_reference(codex_manifest_path, codex_manifest),
            },
        )
    )
    return dict(sorted(files.items()))


def _contract_pins() -> bytes:
    public_manifest = PUBLIC_CONTRACT_MANIFEST.read_bytes()
    return canonical_json(
        envelope(
            "openai-responses-contract-pins-v1",
            "corpus",
            "contract_pins",
            {
                "public_contract": {
                    "manifest": file_reference(
                        PUBLIC_MANIFEST_REPOSITORY_PATH, public_manifest
                    )
                },
                "codex_source": {
                    "commit": CODEX_COMMIT,
                    "sources": [
                        {"path": path, "blob": blob} for path, blob in CODEX_SOURCES
                    ],
                },
            },
        )
    )


def _backend_matrix() -> bytes:
    entries = []
    for index, concern in enumerate(BACKEND_CONCERNS, start=1):
        suffix = f"{index:03d}"
        entries.append(
            {
                "concern": concern,
                "public_contract": f"norn-synthetic-public-contract-{suffix}",
                "codex_overlay": f"norn-synthetic-codex-overlay-{suffix}",
                "p1_treatment": f"norn-synthetic-p1-treatment-{suffix}",
            }
        )
    return canonical_json(
        envelope(
            "openai-responses-backend-state-matrix-v1",
            "corpus",
            "backend_state_matrix",
            {"entries": entries},
        )
    )


def write_corpus(files: dict[str, bytes]) -> None:
    existing = existing_fixture_files()
    unexpected = existing.difference(files)
    if unexpected:
        raise CorpusError("fixture root contains files not owned by this generator")
    for repository_path, value in files.items():
        path = REPOSITORY_ROOT / repository_path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(value)


def check_corpus(files: dict[str, bytes]) -> None:
    existing = existing_fixture_files()
    expected = set(files)
    if existing != expected:
        raise CorpusError("checked-in fixture path inventory does not match the generator")
    for repository_path, expected_bytes in files.items():
        if (REPOSITORY_ROOT / repository_path).read_bytes() != expected_bytes:
            raise CorpusError("checked-in fixture bytes do not match the generator")


def existing_fixture_files() -> set[str]:
    if not FIXTURE_ROOT.exists():
        return set()
    return {
        path.relative_to(REPOSITORY_ROOT).as_posix()
        for path in FIXTURE_ROOT.rglob("*")
        if path.is_file()
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--write", action="store_true")
    mode.add_argument("--check", action="store_true")
    args = parser.parse_args()
    try:
        files = build_corpus()
        if args.write:
            write_corpus(files)
        else:
            check_corpus(files)
    except (CorpusError, OSError, ValueError, json.JSONDecodeError) as error:
        print(f"responses fixture corpus: FAIL: {error}")
        return 1
    print(f"responses fixture corpus: PASS ({len(files)} files)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
