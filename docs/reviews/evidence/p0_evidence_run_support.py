"""Small result and inventory helpers for the integrated evidence runner."""

from __future__ import annotations

import json
from pathlib import Path

import p0_evidence_manifest as manifest
from p0_evidence_support import Case


def validate_distribution_inventory(
    cases: list[Case], concurrency_runs: int, other_runs: int
) -> None:
    if len({case.case_id for case in cases}) != len(cases):
        raise RuntimeError("P0 distribution case identifiers must be unique")
    actual = []
    for case in cases:
        profile = "concurrency" if case.group == "macos_concurrency" else "other"
        expected_runs = concurrency_runs if profile == "concurrency" else other_runs
        if case.runs != expected_runs:
            raise RuntimeError(
                f"unexpected run count for distribution case {case.case_id}"
            )
        actual.append(
            (
                case.case_id,
                case.group,
                profile,
                case.expected_tests,
                case.expected_test_names,
            )
        )
    if tuple(actual) != manifest.DISTRIBUTION_INVENTORY:
        raise RuntimeError("P0 distribution identity inventory changed")


def write_result(path: Path, result: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")


def rust_test_executions(records: list[dict[str, object]]) -> int:
    return sum(
        sum(observation.get("test_counts", {}).values())
        for record in records
        for observation in record["observations"]
    )
