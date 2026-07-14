#!/usr/bin/env python3
"""Bind clean-head P0 Gate C and distribution artifacts into one verdict."""

from __future__ import annotations

import argparse
import hashlib
import importlib
import json
import re
import shutil
import sys
from pathlib import Path
from typing import Final

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
manifest = importlib.import_module("p0_evidence_manifest")
runner = importlib.import_module("run_p0_integrated_evidence")
support = importlib.import_module("p0_evidence_support")
Case = support.Case

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
    "rustc",
    "cargo",
)
SHA256_PATTERN: Final = re.compile(r"^[0-9a-f]{64}$")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gate", required=True, type=Path)
    parser.add_argument("--distributions", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    return parser.parse_args()


def repository_root() -> Path:
    return Path(__file__).resolve().parents[3]


def load_artifact(path: Path) -> tuple[dict[str, object], str]:
    raw = path.read_bytes()
    payload = support.strict_json_loads(raw)
    if not isinstance(payload, dict):
        raise RuntimeError(f"evidence artifact must contain an object: {path}")
    return payload, hashlib.sha256(raw).hexdigest()


def plain_integer(value: object) -> bool:
    return type(value) is int


def contract_equal(actual: object, expected: object) -> bool:
    return type(actual) is type(expected) and actual == expected


def digest_is_valid(value: object) -> bool:
    return (
        isinstance(value, dict)
        and set(value) == {"bytes", "sha256"}
        and plain_integer(value.get("bytes"))
        and value["bytes"] >= 0
        and isinstance(value.get("sha256"), str)
        and SHA256_PATTERN.fullmatch(value["sha256"]) is not None
    )


def counts_are_valid(value: object) -> bool:
    return (
        isinstance(value, dict)
        and set(value) == {"passed", "failed", "ignored"}
        and all(plain_integer(value[name]) and value[name] >= 0 for name in value)
    )


def observation_errors(
    observation: object, case: Case, label: str
) -> tuple[list[str], bool, int]:
    if not isinstance(observation, dict):
        return [f"{label}: observation is not an object"], False, 0
    errors = []
    if not contract_equal(observation.get("exit_code"), 0):
        errors.append(f"{label}: observation exited nonzero")
    duration = observation.get("duration_ms")
    if not plain_integer(duration) or duration < 0:
        errors.append(f"{label}: invalid observation duration")
    for stream in ("stdout", "stderr"):
        if not digest_is_valid(observation.get(stream)):
            errors.append(f"{label}: invalid {stream} digest")
    if "contract_failure" in observation:
        errors.append(f"{label}: retained observation has a contract failure")

    counts = observation.get("test_counts")
    executions = 0
    if counts_are_valid(counts):
        executions = sum(counts.values())
    elif case.expected_tests is not None or case.minimum_tests > 0:
        errors.append(f"{label}: required test counts are missing or malformed")
    elif counts is not None:
        errors.append(f"{label}: optional test counts are malformed")

    if counts_are_valid(counts):
        if case.expected_tests is not None and counts != {
            "passed": case.expected_tests,
            "failed": 0,
            "ignored": 0,
        }:
            errors.append(f"{label}: exact test count contract failed")
        if case.minimum_tests > 0 and (
            counts["passed"] < case.minimum_tests
            or counts["failed"] != 0
            or counts["ignored"] != 0
        ):
            errors.append(f"{label}: minimum test count contract failed")

    names = observation.get("test_names")
    if case.expected_test_names:
        expected_names = tuple(sorted(case.expected_test_names))
        if (
            not isinstance(names, list)
            or not all(isinstance(name, str) for name in names)
            or tuple(sorted(names)) != expected_names
        ):
            errors.append(f"{label}: exact test identity contract failed")
    elif names is not None and (
        not isinstance(names, list) or not all(isinstance(name, str) for name in names)
    ):
        errors.append(f"{label}: malformed optional test identities")

    derived_pass = not errors
    if observation.get("passed") is not derived_pass:
        errors.append(f"{label}: observation pass field disagrees with raw contract")
        derived_pass = False
    return errors, derived_pass, executions


def case_errors(
    record: object, case: Case, label: str
) -> tuple[list[str], int, int, int]:
    if not isinstance(record, dict):
        return [f"{label}: case record is not an object"], 0, 1, 0
    errors = []
    expected_fields = {
        "id": case.case_id,
        "group": case.group,
        "command": list(case.command),
        "runs": case.runs,
        "expected_tests": case.expected_tests,
        "minimum_tests": case.minimum_tests,
        "expected_test_names": list(case.expected_test_names),
    }
    for field, expected in expected_fields.items():
        if not contract_equal(record.get(field), expected):
            errors.append(f"{label}: {field} differs from the pinned contract")

    observations = record.get("observations")
    if not isinstance(observations, list) or len(observations) != case.runs:
        errors.append(f"{label}: observation count differs from the pinned contract")
        observations = observations if isinstance(observations, list) else []
    passed = 0
    executions = 0
    for index, observation in enumerate(observations):
        observation_failures, did_pass, observed_executions = observation_errors(
            observation, case, f"{label} run {index + 1}"
        )
        errors.extend(observation_failures)
        passed += int(did_pass)
        executions += observed_executions
    failed = case.runs - passed
    if not contract_equal(record.get("passed"), passed) or not contract_equal(
        record.get("failed"), failed
    ):
        errors.append(f"{label}: case totals disagree with raw observations")
    return errors, passed, failed, executions


def exact_case_errors(
    payload: dict[str, object], expected: list[Case], label: str
) -> tuple[list[str], int]:
    errors = []
    records = payload.get("cases")
    if not isinstance(records, list):
        return [f"{label}: cases must be a list"], 0
    if len(records) != len(expected):
        errors.append(f"{label}: exact case count changed")
    passed = 0
    failed = 0
    executions = 0
    for index, (record, case) in enumerate(zip(records, expected)):
        failures, case_passed, case_failed, case_executions = case_errors(
            record, case, f"{label} case {index + 1} ({case.case_id})"
        )
        errors.extend(failures)
        passed += case_passed
        failed += case_failed
        executions += case_executions
    expected_runs = sum(case.runs for case in expected)
    if not contract_equal(payload.get("runner_observations"), expected_runs):
        errors.append(f"{label}: top-level observation total is inconsistent")
    if not contract_equal(payload.get("passed"), passed) or not contract_equal(
        payload.get("failed"), failed
    ):
        errors.append(f"{label}: top-level pass totals disagree with observations")
    if not contract_equal(payload.get("rust_test_executions"), executions):
        errors.append(f"{label}: Rust execution total disagrees with observations")
    if payload.get("repository_integrity_passed") is not True:
        errors.append(f"{label}: repository integrity did not pass")
    expected_state = {"head": payload.get("head"), "worktree_status": ""}
    if payload.get("worktree_status") != "":
        errors.append(f"{label}: evidence did not start from a clean worktree")
    if payload.get("final_repository_state") != expected_state:
        errors.append(f"{label}: evidence did not finish at the same clean head")
    return errors, executions


def policy_output_path(
    payload: dict[str, object], root: Path
) -> tuple[Path | None, list[str]]:
    errors = []
    records = payload.get("cases")
    matching = (
        [
            record
            for record in records
            if isinstance(record, dict) and record.get("id") == "full_range_policy"
        ]
        if isinstance(records, list)
        else []
    )
    if len(matching) != 1:
        return None, ["gate: full-range policy case is not unique"]
    command = matching[0].get("command")
    if (
        not isinstance(command, list)
        or len(command) != 10
        or not all(isinstance(item, str) for item in command)
        or command[-2] != "--output"
    ):
        return None, ["gate: policy command shape is invalid"]
    path = Path(command[-1])
    if not path.is_absolute():
        errors.append("gate: policy output path is not absolute")
    try:
        support.require_external_path(root, path, "policy output")
    except RuntimeError as error:
        errors.append(f"gate: {error}")
    return path, errors


def gate_errors(payload: dict[str, object], root: Path) -> list[str]:
    errors = []
    if payload.get("kind") != "gate":
        errors.append("gate: wrong artifact kind")
    head = payload.get("head")
    python = sys.executable
    policy_path, path_errors = policy_output_path(payload, root)
    errors.extend(path_errors)
    if not isinstance(head, str) or policy_path is None:
        errors.append("gate: cannot reconstruct the pinned command contract")
        return errors
    expected = runner.gate_cases(policy_path, head, python)
    if tuple(case.case_id for case in expected) != manifest.GATE_CASE_IDS:
        errors.append("gate: checked-in case inventory is internally inconsistent")
    case_failures, _ = exact_case_errors(payload, expected, "gate")
    errors.extend(case_failures)
    artifact = payload.get("policy_artifact")
    if (
        not isinstance(artifact, dict)
        or artifact.get("file_name") != policy_path.name
        or not contract_equal(artifact.get("schema_version"), 1)
        or artifact.get("head") != head
        or artifact.get("policy_passed") is not True
        or not isinstance(artifact.get("sha256"), str)
        or SHA256_PATTERN.fullmatch(artifact["sha256"]) is None
    ):
        errors.append("gate: policy artifact summary is invalid")
    rebound, rebound_passed = runner.policy_contract.bind_policy_artifact(
        policy_path, root, head, BASE
    )
    if artifact != rebound or not rebound_passed:
        errors.append("gate: policy artifact does not rebind to its retained file")
    if not runner.policy_contract.independently_reproduces(
        policy_path, root, head, BASE, python
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
        if all(plain_integer(value) for value in concurrency_values)
        else set()
    )
    other = (
        set(other_values)
        if all(plain_integer(value) for value in other_values)
        else set()
    )
    if len(concurrency) != 1:
        errors.append("distributions: concurrency run profile is inconsistent")
        concurrency_runs = MINIMUM_CONCURRENCY_RUNS
    else:
        concurrency_runs = next(iter(concurrency))
    if len(other) != 1:
        errors.append("distributions: repeated run profile is inconsistent")
        other_runs = MINIMUM_OTHER_RUNS
    else:
        other_runs = next(iter(other))
    if concurrency_runs < MINIMUM_CONCURRENCY_RUNS:
        errors.append("distributions: concurrency run minimum was not met")
    if other_runs < MINIMUM_OTHER_RUNS:
        errors.append("distributions: repeated run minimum was not met")
    return concurrency_runs, other_runs


def distribution_errors(payload: dict[str, object]) -> tuple[list[str], int]:
    errors = []
    if payload.get("kind") != "distributions":
        errors.append("distributions: wrong artifact kind")
    concurrency_runs, other_runs = distribution_profiles(payload, errors)
    expected = runner.distribution_cases(concurrency_runs, other_runs)
    try:
        runner.validate_distribution_inventory(expected, concurrency_runs, other_runs)
    except RuntimeError as error:
        errors.append(f"distributions: {error}")
    case_failures, executions = exact_case_errors(payload, expected, "distributions")
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


def executable_errors(payload: dict[str, object], label: str) -> list[str]:
    errors = []
    fingerprint = payload.get("environment_fingerprint")
    executables = (
        fingerprint.get("executables") if isinstance(fingerprint, dict) else None
    )
    if not isinstance(executables, dict):
        return [f"{label}: executable fingerprint is missing"]
    for name in ("cargo", "git", "python", "rustc"):
        record = executables.get(name)
        declared = record.get("path") if isinstance(record, dict) else None
        resolved = record.get("resolved_path") if isinstance(record, dict) else None
        digest = record.get("sha256") if isinstance(record, dict) else None
        if (
            not isinstance(record, dict)
            or not isinstance(declared, str)
            or not isinstance(resolved, str)
            or not isinstance(digest, str)
            or SHA256_PATTERN.fullmatch(digest) is None
        ):
            errors.append(f"{label}: invalid {name} executable fingerprint")
            continue
        path = Path(resolved)
        current = sys.executable if name == "python" else shutil.which(name)
        if (
            not path.is_file()
            or current is None
            or Path(current).resolve() != path.resolve()
            or Path(declared).resolve() != path.resolve()
            or support.sha256_file(path) != digest
            or (name == "python" and path.resolve() != Path(sys.executable).resolve())
        ):
            errors.append(f"{label}: {name} executable no longer matches its digest")
    return errors


def main() -> int:
    args = parse_args()
    root = repository_root()
    for path, label in (
        (args.gate, "gate input"),
        (args.distributions, "distribution input"),
        (args.output, "attestation output"),
    ):
        support.require_external_path(root, path, label)
    resolved = {path.resolve() for path in (args.gate, args.distributions, args.output)}
    if len(resolved) != 3:
        raise RuntimeError("gate, distribution, and attestation paths must be distinct")
    gate, gate_hash = load_artifact(args.gate)
    distributions, distribution_hash = load_artifact(args.distributions)
    errors = gate_errors(gate, root)
    policy_path, _path_errors = policy_output_path(gate, root)
    if policy_path is not None and args.output.resolve() == policy_path.resolve():
        raise RuntimeError("attestation output aliases the retained policy artifact")
    distribution_failures, minimum_rust_executions = distribution_errors(distributions)
    errors.extend(distribution_failures)
    errors.extend(executable_errors(gate, "gate"))
    errors.extend(executable_errors(distributions, "distributions"))
    if not contract_equal(gate.get("schema_version"), 2) or not contract_equal(
        distributions.get("schema_version"), 2
    ):
        errors.append("artifacts do not use the required evidence schema")
    if gate.get("base") != BASE or distributions.get("base") != BASE:
        errors.append("artifacts do not bind the required base")
    for field in COMMON_FIELDS:
        if gate.get(field) != distributions.get(field):
            errors.append(f"artifacts differ on {field}")
    if gate.get("cargo_config") != distributions.get("cargo_config"):
        errors.append("artifacts differ on Cargo configuration")
    gate_fingerprint = gate.get("environment_fingerprint")
    distribution_fingerprint = distributions.get("environment_fingerprint")
    gate_executables = (
        gate_fingerprint.get("executables")
        if isinstance(gate_fingerprint, dict)
        else None
    )
    distribution_executables = (
        distribution_fingerprint.get("executables")
        if isinstance(distribution_fingerprint, dict)
        else None
    )
    if not gate_executables or gate_executables != distribution_executables:
        errors.append("artifacts differ on executable identities")
    state = support.repository_state(root)
    if state != {"head": gate.get("head"), "worktree_status": ""}:
        errors.append("attester is not running at the artifacts' clean head")
    base_commit = support.checked_output(root, "git", "rev-parse", BASE)
    if gate.get("base_commit") != base_commit:
        errors.append("artifacts do not bind the resolved base commit")
    result = {
        "schema_version": 1,
        "kind": "p0_final_attestation",
        "passed": not errors,
        "errors": errors,
        "base": gate.get("base"),
        "head": gate.get("head"),
        "gate": {"file_name": args.gate.name, "sha256": gate_hash},
        "distributions": {
            "file_name": args.distributions.name,
            "sha256": distribution_hash,
            "case_count": len(manifest.DISTRIBUTION_INVENTORY),
            "minimum_runner_observations": MINIMUM_DISTRIBUTION_OBSERVATIONS,
            "minimum_rust_test_executions": minimum_rust_executions,
        },
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
