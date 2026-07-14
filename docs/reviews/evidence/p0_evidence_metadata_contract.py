"""Exact, disclosure-safe metadata schema for P0 retained evidence."""

from __future__ import annotations

import datetime as dt
import re
from typing import Final

import p0_evidence_disclosure as disclosure
import p0_evidence_paths as paths
import p0_evidence_support as support


SHA256_PATTERN: Final = re.compile(r"^[0-9a-f]{64}$")
COMMON_METADATA_KEYS: Final = frozenset(
    {
        "schema_version",
        "kind",
        "generated_at_utc",
        "base",
        "base_commit",
        "head",
        "worktree_clean",
        "platform_system",
        "platform",
        "python",
        "rustc",
        "cargo",
        "environment",
        "environment_fingerprint",
        "cargo_config",
        "temporary_filesystem",
        "logical_cpu_count",
        "storage_layout",
    }
)
RESULT_KEYS: Final = frozenset(
    {
        "cases",
        "passed",
        "failed",
        "runner_observations",
        "rust_test_executions",
        "final_repository_state",
        "repository_integrity_passed",
    }
)
GATE_EXTRA_KEYS: Final = frozenset({"policy_artifact", "policy_contract_passed"})


def metadata_errors(payload: dict[str, object], kind: str, label: str) -> list[str]:
    expected_keys = COMMON_METADATA_KEYS | RESULT_KEYS
    if kind == "gate":
        expected_keys |= GATE_EXTRA_KEYS
    errors = _exact_keys(payload, expected_keys, label)
    if not _contract_equal(payload.get("schema_version"), 3):
        errors.append(f"{label}: wrong evidence schema version")
    if payload.get("kind") != kind:
        errors.append(f"{label}: wrong artifact kind")
    if payload.get("storage_layout") != paths.STORAGE_LAYOUT:
        errors.append(f"{label}: wrong repository-local storage layout")
    timestamp = payload.get("generated_at_utc")
    try:
        parsed = (
            dt.datetime.fromisoformat(timestamp) if isinstance(timestamp, str) else None
        )
    except ValueError:
        parsed = None
    if parsed is None or parsed.tzinfo is None or parsed.utcoffset() != dt.timedelta(0):
        errors.append(f"{label}: invalid UTC generation timestamp")
    cpu_count = payload.get("logical_cpu_count")
    if not _plain_integer(cpu_count) or cpu_count <= 0:
        errors.append(f"{label}: invalid logical CPU count")
    for field in (
        "base",
        "base_commit",
        "head",
        "platform_system",
        "platform",
        "python",
        "rustc",
        "cargo",
    ):
        if not isinstance(payload.get(field), str):
            errors.append(f"{label}: {field} must be a string")
    if payload.get("worktree_clean") is not True:
        errors.append(f"{label}: evidence worktree was not clean")
    errors.extend(_environment_errors(payload.get("environment"), label))
    errors.extend(_fingerprint_errors(payload.get("environment_fingerprint"), label))
    errors.extend(_cargo_config_errors(payload.get("cargo_config"), label))
    temporary = payload.get("temporary_filesystem")
    if (
        not isinstance(temporary, dict)
        or set(temporary) != {"filesystem"}
        or not isinstance(temporary.get("filesystem"), str)
    ):
        errors.append(f"{label}: temporary filesystem record is malformed")
    if disclosure.contains_absolute_path(payload):
        errors.append(f"{label}: retained absolute path is forbidden")
    return errors


def _environment_errors(value: object, label: str) -> list[str]:
    expected_keys = set(support.PATH_FREE_ENVIRONMENT_CONTROLS) | {
        "sanitized_variable_names",
        "removed_ambient_variable_count",
    }
    if not isinstance(value, dict):
        return [f"{label}: environment record is missing"]
    errors = _exact_keys(value, expected_keys, f"{label} environment")
    for name, expected in support.PATH_FREE_ENVIRONMENT_CONTROLS.items():
        if value.get(name) != expected:
            errors.append(f"{label}: environment control {name} changed")
    if value.get("sanitized_variable_names") != list(support.SANITIZED_VARIABLE_NAMES):
        errors.append(f"{label}: sanitized environment allowlist changed")
    removed = value.get("removed_ambient_variable_count")
    if not _plain_integer(removed) or removed < 0:
        errors.append(f"{label}: removed ambient variable count is invalid")
    return errors


def _fingerprint_errors(value: object, label: str) -> list[str]:
    if not isinstance(value, dict) or set(value) != {"executables"}:
        return [f"{label}: executable fingerprint record is malformed"]
    executables = value.get("executables")
    if not isinstance(executables, dict):
        return [f"{label}: executable fingerprints are missing"]
    errors = _exact_keys(executables, set(support.EXECUTABLE_NAMES), f"{label} tools")
    for name in support.EXECUTABLE_NAMES:
        record = executables.get(name)
        digest = record.get("sha256") if isinstance(record, dict) else None
        if not isinstance(record, dict) or set(record) != {"sha256"}:
            errors.append(f"{label}: invalid {name} fingerprint shape")
        elif digest is not None and (
            not isinstance(digest, str) or SHA256_PATTERN.fullmatch(digest) is None
        ):
            errors.append(f"{label}: invalid {name} digest")
    for required in ("cargo", "git", "python", "rustc", "rustup"):
        record = executables.get(required)
        if not isinstance(record, dict) or not isinstance(record.get("sha256"), str):
            errors.append(f"{label}: required {required} digest is missing")
    return errors


def _cargo_config_errors(value: object, label: str) -> list[str]:
    if not isinstance(value, dict):
        return [f"{label}: Cargo configuration record is missing"]
    errors = _exact_keys(value, {"repository", "user"}, f"{label} Cargo config")
    for name in ("repository", "user"):
        record = value.get(name)
        if not isinstance(record, dict) or set(record) != {"present", "sha256"}:
            errors.append(f"{label}: malformed {name} Cargo config record")
            continue
        present = record.get("present")
        digest = record.get("sha256")
        if type(present) is not bool or (present != (digest is not None)):
            errors.append(f"{label}: inconsistent {name} Cargo config record")
        if digest is not None and (
            not isinstance(digest, str) or SHA256_PATTERN.fullmatch(digest) is None
        ):
            errors.append(f"{label}: invalid {name} Cargo config digest")
    return errors


def _exact_keys(
    value: dict[str, object], expected: set[str] | frozenset[str], label: str
) -> list[str]:
    errors = []
    extra = set(value) - expected
    missing = expected - set(value)
    if extra:
        errors.append(f"{label}: unexpected keys {sorted(extra)}")
    if missing:
        errors.append(f"{label}: missing keys {sorted(missing)}")
    return errors


def _plain_integer(value: object) -> bool:
    return type(value) is int


def _contract_equal(actual: object, expected: object) -> bool:
    return type(actual) is type(expected) and actual == expected
