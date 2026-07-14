"""Exact schema checks shared by the P0 evidence attester."""

from __future__ import annotations

import re
from typing import Final

import p0_evidence_metadata_contract as metadata_contract
import p0_evidence_support as support
from p0_test_output import (
    stable_test_identity,
    stable_test_summary,
    stable_test_target_record,
)


SHA256_PATTERN: Final = re.compile(r"^[0-9a-f]{64}$")
CASE_KEYS: Final = frozenset(
    {
        "id",
        "group",
        "command",
        "runs",
        "expected_tests",
        "minimum_tests",
        "expected_test_names",
        "expected_tool_tests",
        "expected_tool_test_modules",
        "passed",
        "failed",
        "observations",
    }
)
OBSERVATION_KEYS: Final = frozenset(
    {
        "exit_code",
        "duration_ms",
        "stdout",
        "stderr",
        "test_summaries",
        "test_counts",
        "test_names",
        "tool_test_count",
        "failed_test_names",
        "failed_test_identity_groups",
        "failed_test_identity_complete",
        "contract_failure",
        "passed",
    }
)
CONTRACT_FAILURES: Final = frozenset(
    {
        "exact_test_count",
        "minimum_test_count",
        "exact_test_identity",
        "unexpected_failure_identity",
        "exact_tool_test_count",
    }
)


def plain_integer(value: object) -> bool:
    return type(value) is int


def metadata_errors(payload: dict[str, object], kind: str, label: str) -> list[str]:
    return metadata_contract.metadata_errors(payload, kind, label)


def contract_equal(actual: object, expected: object) -> bool:
    return type(actual) is type(expected) and actual == expected


def observation_errors(
    observation: object, case: support.Case, label: str
) -> tuple[list[str], bool, int]:
    if not isinstance(observation, dict):
        return [f"{label}: observation is not an object"], False, 0
    errors = exact_keys(observation, OBSERVATION_KEYS, label, allow_missing=True)
    if not contract_equal(observation.get("exit_code"), 0):
        errors.append(f"{label}: observation exited nonzero")
    duration = observation.get("duration_ms")
    if not plain_integer(duration) or duration < 0:
        errors.append(f"{label}: invalid observation duration")
    for stream in ("stdout", "stderr"):
        if not digest_is_valid(observation.get(stream)):
            errors.append(f"{label}: invalid {stream} digest")

    summaries = observation.get("test_summaries")
    counts = observation.get("test_counts")
    if (summaries is None) != (counts is None):
        errors.append(f"{label}: test summaries and counts must appear together")
    if summaries is not None:
        if not isinstance(summaries, list) or not all(
            isinstance(line, str) and stable_test_summary(line) for line in summaries
        ):
            errors.append(f"{label}: malformed test summaries")
        elif support.test_counts(summaries) != counts:
            errors.append(f"{label}: test counts disagree with retained summaries")
    executions = sum(counts.values()) if counts_are_valid(counts) else 0
    if not counts_are_valid(counts):
        if case.expected_tests is not None or case.minimum_tests > 0:
            errors.append(f"{label}: required test counts are missing or malformed")
        elif counts is not None:
            errors.append(f"{label}: optional test counts are malformed")
    else:
        errors.extend(test_count_contract_errors(counts, case, label))
    tool_count = observation.get("tool_test_count")
    if case.expected_tool_tests is None:
        if "tool_test_count" in observation:
            errors.append(f"{label}: unexpected tool test count")
    elif not contract_equal(tool_count, case.expected_tool_tests):
        errors.append(f"{label}: exact tool test count contract failed")

    names = observation.get("test_names")
    if names is not None and (
        not isinstance(names, list)
        or not all(
            isinstance(name, str) and stable_test_identity(name) for name in names
        )
    ):
        errors.append(f"{label}: malformed optional test identities")
    if case.expected_test_names and (
        not isinstance(names, list)
        or tuple(sorted(names)) != tuple(sorted(case.expected_test_names))
    ):
        errors.append(f"{label}: exact test identity contract failed")
    errors.extend(failure_identity_errors(observation, counts, label))

    contract_failure = observation.get("contract_failure")
    if contract_failure is not None:
        if contract_failure not in CONTRACT_FAILURES:
            errors.append(f"{label}: unknown contract failure code")
        else:
            errors.append(f"{label}: retained observation has a contract failure")
    derived_pass = not errors
    if observation.get("passed") is not derived_pass:
        errors.append(f"{label}: observation pass field disagrees with raw contract")
        derived_pass = False
    required = {"exit_code", "duration_ms", "stdout", "stderr", "passed"}
    missing = required - set(observation)
    if missing:
        errors.append(f"{label}: missing required keys {sorted(missing)}")
        derived_pass = False
    return errors, derived_pass, executions


def failure_identity_errors(
    observation: dict[str, object], counts: object, label: str
) -> list[str]:
    fields = {
        "failed_test_names",
        "failed_test_identity_groups",
        "failed_test_identity_complete",
    }
    present = fields & set(observation)
    failed_count = counts.get("failed") if counts_are_valid(counts) else None
    if not present:
        return (
            [f"{label}: failed test identities are missing"]
            if failed_count is not None and failed_count > 0
            else []
        )
    if present != fields:
        return [f"{label}: failed test identity fields are incomplete"]
    names = observation.get("failed_test_names")
    groups = observation.get("failed_test_identity_groups")
    errors = []
    if not isinstance(names, list) or not all(
        isinstance(name, str) and stable_test_identity(name) for name in names
    ):
        errors.append(f"{label}: failed test names are malformed")
        names = []
    flattened: list[str] = []
    targets: list[tuple[str, str, str | None]] = []
    complete_groups = True
    if not isinstance(groups, list) or not groups:
        errors.append(f"{label}: failed test identity groups are missing")
        complete_groups = False
    else:
        for index, group in enumerate(groups):
            group_errors, group_names, complete, target = failure_group_errors(
                group, f"{label} failure group {index + 1}"
            )
            errors.extend(group_errors)
            flattened.extend(group_names)
            complete_groups = complete_groups and complete
            if target is not None:
                targets.append(target)
    if len(set(targets)) != len(targets):
        errors.append(f"{label}: duplicate test target identities")
        complete_groups = False
    if flattened != names:
        errors.append(f"{label}: flattened failure groups disagree with names")
    computed_complete = (
        complete_groups
        and failed_count is not None
        and failed_count > 0
        and len(flattened) == failed_count
    )
    if observation.get("failed_test_identity_complete") is not computed_complete:
        errors.append(f"{label}: failed identity completeness flag is invalid")
    if failed_count is not None and failed_count > 0 and not computed_complete:
        errors.append(f"{label}: failed test identities are incomplete")
    if failed_count is None or failed_count == 0:
        errors.append(f"{label}: failure identities lack a failed-test count")
    return errors


def failure_group_errors(
    value: object, label: str
) -> tuple[list[str], list[str], bool, tuple[str, str, str | None] | None]:
    if not isinstance(value, dict):
        return [f"{label}: group is not an object"], [], False, None
    errors = exact_keys(value, {"source", "declared_failed", "target", "names"}, label)
    names = value.get("names")
    if not isinstance(names, list) or not all(
        isinstance(name, str) and stable_test_identity(name) for name in names
    ):
        errors.append(f"{label}: names are malformed")
        names = []
    if len(set(names)) != len(names):
        errors.append(f"{label}: duplicate names within one test binary")
    source = value.get("source")
    declared = value.get("declared_failed")
    target_value = value.get("target")
    target = (
        (
            target_value["package"],
            target_value["kind"],
            target_value["name"],
        )
        if stable_test_target_record(target_value)
        else None
    )
    if source == "summary":
        if target is None:
            errors.append(f"{label}: stable test target identity is missing")
        complete = (
            plain_integer(declared)
            and declared > 0
            and target is not None
            and len(names) == declared
            and len(set(names)) == len(names)
        )
    elif source == "status_fallback":
        complete = False
        if declared is not None or target_value is not None or len(names) != 1:
            errors.append(f"{label}: malformed status fallback")
    else:
        complete = False
        errors.append(f"{label}: unknown identity source")
    return errors, names, complete, target


def test_count_contract_errors(
    counts: dict[str, int], case: support.Case, label: str
) -> list[str]:
    errors = []
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
    return errors


def case_errors(
    record: object, case: support.Case, label: str
) -> tuple[list[str], int, int, int]:
    if not isinstance(record, dict):
        return [f"{label}: case record is not an object"], 0, 1, 0
    errors = exact_keys(record, CASE_KEYS, label)
    expected_fields = {
        "id": case.case_id,
        "group": case.group,
        "command": support.case_contract_command(case),
        "runs": case.runs,
        "expected_tests": case.expected_tests,
        "minimum_tests": case.minimum_tests,
        "expected_test_names": list(case.expected_test_names),
        "expected_tool_tests": case.expected_tool_tests,
        "expected_tool_test_modules": list(case.expected_tool_test_modules),
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
        failures, did_pass, observed = observation_errors(
            observation, case, f"{label} run {index + 1}"
        )
        errors.extend(failures)
        passed += int(did_pass)
        executions += observed
    failed = case.runs - passed
    if not contract_equal(record.get("passed"), passed) or not contract_equal(
        record.get("failed"), failed
    ):
        errors.append(f"{label}: case totals disagree with raw observations")
    return errors, passed, failed, executions


def exact_case_errors(
    payload: dict[str, object], expected: list[support.Case], label: str
) -> tuple[list[str], int]:
    errors = []
    records = payload.get("cases")
    if not isinstance(records, list):
        return [f"{label}: cases must be a list"], 0
    if len(records) != len(expected):
        errors.append(f"{label}: exact case count changed")
    passed = 0
    executions = 0
    for index, (record, case) in enumerate(zip(records, expected)):
        failures, case_passed, _case_failed, case_executions = case_errors(
            record, case, f"{label} case {index + 1} ({case.case_id})"
        )
        errors.extend(failures)
        passed += case_passed
        executions += case_executions
    expected_runs = sum(case.runs for case in expected)
    failed = expected_runs - passed
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
    expected_state = {"head": payload.get("head"), "worktree_clean": True}
    if payload.get("worktree_clean") is not True:
        errors.append(f"{label}: evidence did not start from a clean worktree")
    if payload.get("final_repository_state") != expected_state:
        errors.append(f"{label}: evidence did not finish at the same clean head")
    return errors, executions


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


def exact_keys(
    value: dict[str, object],
    expected: set[str] | frozenset[str],
    label: str,
    *,
    allow_missing: bool = False,
) -> list[str]:
    actual = set(value)
    errors = []
    extra = actual - expected
    missing = expected - actual
    if extra:
        errors.append(f"{label}: unexpected keys {sorted(extra)}")
    if missing and not allow_missing:
        errors.append(f"{label}: missing keys {sorted(missing)}")
    return errors
