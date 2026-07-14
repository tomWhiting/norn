#!/usr/bin/env python3
"""Bind clean-head P0 Gate C and distribution artifacts into one verdict."""

from __future__ import annotations

import argparse
import hashlib
import importlib
import json
import os
import sys
from pathlib import Path
from typing import Final

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
contract = importlib.import_module("p0_evidence_attestation_contract")
manifest = importlib.import_module("p0_evidence_manifest")
paths = importlib.import_module("p0_evidence_paths")
policy_contract = importlib.import_module("p0_evidence_policy")
run_support = importlib.import_module("p0_evidence_run_support")
runner = importlib.import_module("run_p0_integrated_evidence")
support = importlib.import_module("p0_evidence_support")


BASE: Final = "41ea210"
MINIMUM_CONCURRENCY_RUNS: Final = 50
MINIMUM_OTHER_RUNS: Final = 20
MINIMUM_DISTRIBUTION_OBSERVATIONS: Final = 750
COMMON_FIELDS: Final = (
    "schema_version",
    "base",
    "base_commit",
    "head",
    "platform_system",
    "platform",
    "python",
    "rustc",
    "cargo",
    "storage_layout",
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gate", required=True, type=Path)
    parser.add_argument("--distributions", required=True, type=Path)
    parser.add_argument("--policy", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    return parser.parse_args()


def repository_root() -> Path:
    return Path(__file__).resolve().parents[3]


def load_artifact(path: Path) -> tuple[dict[str, object], str]:
    raw = path.read_bytes()
    payload = support.strict_json_loads(raw)
    if not isinstance(payload, dict):
        raise RuntimeError("evidence artifact must contain an object")
    return payload, hashlib.sha256(raw).hexdigest()


def gate_errors(payload: dict[str, object], root: Path, policy_path: Path) -> list[str]:
    errors = contract.metadata_errors(payload, "gate", "gate")
    head = payload.get("head")
    if not isinstance(head, str):
        errors.append("gate: cannot reconstruct the pinned command contract")
        return errors
    expected = runner.gate_cases(policy_path, head, sys.executable)
    if tuple(case.case_id for case in expected) != manifest.GATE_CASE_IDS:
        errors.append("gate: checked-in case inventory is internally inconsistent")
    case_failures, _executions = contract.exact_case_errors(payload, expected, "gate")
    errors.extend(case_failures)
    artifact = payload.get("policy_artifact")
    if (
        not isinstance(artifact, dict)
        or set(artifact) != {"schema_version", "head", "policy_passed", "sha256"}
        or not contract.contract_equal(
            artifact.get("schema_version"), policy_contract.POLICY_SCHEMA_VERSION
        )
        or artifact.get("head") != head
        or artifact.get("policy_passed") is not True
        or not isinstance(artifact.get("sha256"), str)
        or contract.SHA256_PATTERN.fullmatch(artifact["sha256"]) is None
    ):
        errors.append("gate: policy artifact summary is invalid")
    rebound, rebound_passed = policy_contract.bind_policy_artifact(
        policy_path, root, head, BASE
    )
    rebound.pop("file_name", None)
    if artifact != rebound or not rebound_passed:
        errors.append("gate: policy artifact does not rebind to its retained file")
    if not policy_contract.independently_reproduces(
        policy_path, root, head, BASE, sys.executable
    ):
        errors.append("gate: policy artifact differs from independent reproduction")
    if payload.get("policy_contract_passed") is not True:
        errors.append("gate: policy artifact contract did not pass")
    return errors


def distribution_profiles(
    payload: dict[str, object], errors: list[str]
) -> tuple[int, int]:
    records = payload.get("cases")
    if not isinstance(records, list):
        errors.append("distributions: cases must be a list")
        return MINIMUM_CONCURRENCY_RUNS, MINIMUM_OTHER_RUNS
    concurrency_values = [
        record.get("runs")
        for record, expected in zip(records, manifest.DISTRIBUTION_INVENTORY)
        if isinstance(record, dict) and expected[2] == "concurrency"
    ]
    other_values = [
        record.get("runs")
        for record, expected in zip(records, manifest.DISTRIBUTION_INVENTORY)
        if isinstance(record, dict) and expected[2] == "other"
    ]
    concurrency = (
        set(concurrency_values)
        if all(contract.plain_integer(value) for value in concurrency_values)
        else set()
    )
    other = (
        set(other_values)
        if all(contract.plain_integer(value) for value in other_values)
        else set()
    )
    concurrency_runs = _single_profile(
        concurrency,
        MINIMUM_CONCURRENCY_RUNS,
        "concurrency",
        errors,
    )
    other_runs = _single_profile(other, MINIMUM_OTHER_RUNS, "repeated", errors)
    return concurrency_runs, other_runs


def _single_profile(
    values: set[int], minimum: int, label: str, errors: list[str]
) -> int:
    if len(values) != 1:
        errors.append(f"distributions: {label} run profile is inconsistent")
        return minimum
    runs = next(iter(values))
    if runs < minimum:
        errors.append(f"distributions: {label} run minimum was not met")
    return runs


def distribution_errors(payload: dict[str, object]) -> tuple[list[str], int]:
    errors = contract.metadata_errors(payload, "distributions", "distributions")
    concurrency_runs, other_runs = distribution_profiles(payload, errors)
    expected = runner.distribution_cases(concurrency_runs, other_runs)
    try:
        run_support.validate_distribution_inventory(
            expected, concurrency_runs, other_runs
        )
    except RuntimeError as error:
        errors.append(f"distributions: {error}")
    case_failures, executions = contract.exact_case_errors(
        payload, expected, "distributions"
    )
    errors.extend(case_failures)
    expected_observations = sum(case.runs for case in expected)
    minimum_executions = sum(
        case.runs * (case.expected_tests or case.minimum_tests) for case in expected
    )
    if expected_observations < MINIMUM_DISTRIBUTION_OBSERVATIONS:
        errors.append("distributions: observation minimum was not met")
    if executions < minimum_executions:
        errors.append("distributions: Rust execution minimum was not met")
    temporary = payload.get("temporary_filesystem")
    filesystem = temporary.get("filesystem") if isinstance(temporary, dict) else None
    if payload.get("platform_system") != "Darwin" or filesystem != "apfs":
        errors.append("distributions: evidence host was not Darwin on APFS")
    return errors, minimum_executions


def executable_errors(payload: dict[str, object], root: Path, label: str) -> list[str]:
    recorded = payload.get("environment_fingerprint")
    environment = dict(os.environ)
    current = support.environment_fingerprint(environment, runner.TOOLCHAIN)
    errors = []
    if recorded != current:
        errors.append(f"{label}: local executables do not match retained hashes")
    versions = support.toolchain_support.pinned_versions(
        root, environment, runner.TOOLCHAIN
    )
    if payload.get("cargo") != versions["cargo"]:
        errors.append(f"{label}: pinned cargo version does not rebind")
    if payload.get("rustc") != versions["rustc"]:
        errors.append(f"{label}: pinned rustc version does not rebind")
    return errors


def main() -> int:
    args = parse_args()
    root = repository_root()
    validated = paths.validate_attester_paths(
        root, args.gate, args.distributions, args.policy, args.output
    )
    gate, gate_hash = load_artifact(validated.gate)
    distributions, distribution_hash = load_artifact(validated.distributions)
    errors = gate_errors(gate, root, validated.policy)
    distribution_failures, minimum_executions = distribution_errors(distributions)
    errors.extend(distribution_failures)
    errors.extend(executable_errors(gate, root, "gate"))
    errors.extend(executable_errors(distributions, root, "distributions"))
    if gate.get("base") != BASE or distributions.get("base") != BASE:
        errors.append("artifacts do not bind the required base")
    for field in COMMON_FIELDS:
        if gate.get(field) != distributions.get(field):
            errors.append(f"artifacts differ on {field}")
    if gate.get("cargo_config") != distributions.get("cargo_config"):
        errors.append("artifacts differ on Cargo configuration")
    if gate.get("environment_fingerprint") != distributions.get(
        "environment_fingerprint"
    ):
        errors.append("artifacts differ on executable identities")
    state = support.repository_state(root)
    if state != {"head": gate.get("head"), "worktree_clean": True}:
        errors.append("attester is not running at the artifacts' clean head")
    base_commit = support.checked_output(root, "git", "rev-parse", BASE)
    if gate.get("base_commit") != base_commit:
        errors.append("artifacts do not bind the resolved base commit")
    result = {
        "schema_version": 2,
        "kind": "p0_final_attestation",
        "passed": not errors,
        "errors": errors,
        "base": gate.get("base"),
        "head": gate.get("head"),
        "gate": {"sha256": gate_hash},
        "distributions": {
            "sha256": distribution_hash,
            "case_count": len(manifest.DISTRIBUTION_INVENTORY),
            "minimum_runner_observations": MINIMUM_DISTRIBUTION_OBSERVATIONS,
            "minimum_rust_test_executions": minimum_executions,
        },
        "policy": {"sha256": support.sha256_file(validated.policy)},
        "storage_layout": paths.STORAGE_LAYOUT,
    }
    validated.output.parent.mkdir(parents=True, exist_ok=True)
    validated.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
