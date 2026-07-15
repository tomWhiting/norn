"""Closed manifest and evidence semantics for the P1 local gate."""

import hashlib
import re
import stat
from pathlib import Path
from typing import Any

PHASE = "P1"
BASE_COMMIT = "2917c8ed10e7a2ec7ac9c4d7283bafbea7f6577d"
BASE_TREE = "9ae969792c53b4e1dfdc61c6d91f7fe62d3ac582"
ANALYZER_VERSION = "norn-policy-1"
DIGEST_VERSION = "norn-sha256-canonical-json-1"
EXPECTED_COMMANDS = (
    ("rustc-version", "metadata", "@rustc"),
    ("cargo-version", "metadata", "@cargo"),
    ("phase-tests", "phase_test", "@cargo"),
    ("workspace-integration-tests", "integration", "@cargo"),
    ("format", "format", "@cargo"),
    ("clippy", "lint", "@cargo"),
    ("workspace-all-target-tests", "test", "@cargo"),
    ("workspace-doc-tests", "doc_test", "@cargo"),
    ("phase-diff-check", "diff", "@git"),
    ("repository-policy", "policy", "@cargo"),
    ("added-line-audit", "audit", "@support:p1-added-line-audit"),
    ("redaction-check", "redaction", "@support:p1-redaction-check"),
    ("distributions", "distribution", "@support:p1-distributions"),
    ("gate-self-check", "self_check", "@support:p1-gate-self-check"),
)
EXPECTED_PIN_IDS = (
    "orchestrator",
    "environment",
    "contract",
    "evidence",
    "runtime",
    "schema-validator",
    "schema-validator-tests",
    "environment-tests",
    "contract-tests",
    "evidence-tests",
    "runtime-tests",
    "evidence-schema",
    "added-line-audit",
    "redaction-check",
    "distributions",
    "gate-self-check",
)
HEX_40 = re.compile(r"^[0-9a-f]{40}$")
HEX_64 = re.compile(r"^[0-9a-f]{64}$")
SAFE_ID = re.compile(r"^[a-z0-9][a-z0-9-]*$")


class ContractError(Exception):
    """A closed manifest or descriptor contract violation."""


def exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    if set(value) != expected:
        raise ContractError(f"{label} has unknown or missing fields")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def tool_id(token: str) -> str:
    if token in {"@cargo", "@git", "@rustc"}:
        return token[1:]
    prefix = "@support:"
    if token.startswith(prefix) and SAFE_ID.fullmatch(token[len(prefix) :]):
        return token[len(prefix) :]
    raise ContractError("command uses an unknown logical executable")


def validate_manifest(manifest: dict[str, Any]) -> dict[str, dict[str, str]]:
    exact_keys(
        manifest,
        {
            "schema_version",
            "phase",
            "base_commit",
            "implementation",
            "resource_limits",
            "commands",
        },
        "command manifest",
    )
    if (
        type(manifest["schema_version"]) is not int
        or manifest["schema_version"] != 1
        or manifest["phase"] != PHASE
        or manifest["base_commit"] != BASE_COMMIT
    ):
        raise ContractError("command manifest identity is invalid")
    pins = validate_pins(manifest["implementation"])
    validate_resource_limits(manifest["resource_limits"])
    commands = manifest["commands"]
    if not isinstance(commands, list) or len(commands) != len(EXPECTED_COMMANDS):
        raise ContractError("command manifest has the wrong command count")
    for command, expected in zip(commands, EXPECTED_COMMANDS, strict=True):
        if not isinstance(command, dict):
            raise ContractError("command manifest entry must be an object")
        exact_keys(command, {"id", "kind", "argv"}, "command manifest entry")
        identifier, kind, executable = expected
        if command["id"] != identifier or command["kind"] != kind:
            raise ContractError("command manifest differs from the fixed command order")
        argv = command["argv"]
        if (
            not isinstance(argv, list)
            or not argv
            or not all(isinstance(value, str) and value for value in argv)
            or argv[0] != executable
        ):
            raise ContractError("command manifest contains invalid argv")
        tool_id(argv[0])
    return pins


def validate_pins(value: Any) -> dict[str, dict[str, str]]:
    if not isinstance(value, dict):
        raise ContractError("command manifest implementation must be an object")
    exact_keys(value, {"files"}, "command manifest implementation")
    files = value["files"]
    if not isinstance(files, list) or len(files) != len(EXPECTED_PIN_IDS):
        raise ContractError("command manifest has the wrong pinned-file count")
    result: dict[str, dict[str, str]] = {}
    for entry, expected_id in zip(files, EXPECTED_PIN_IDS, strict=True):
        if not isinstance(entry, dict):
            raise ContractError("pinned-file entry must be an object")
        exact_keys(entry, {"id", "path", "sha256", "status"}, "pinned-file entry")
        identifier = entry["id"]
        path = entry["path"]
        digest = entry["sha256"]
        status = entry["status"]
        if identifier != expected_id or not SAFE_ID.fullmatch(identifier):
            raise ContractError("pinned-file order or ID is invalid")
        if not isinstance(path, str) or not safe_relative_path(path):
            raise ContractError("pinned-file path is invalid")
        if not isinstance(digest, str) or not HEX_64.fullmatch(digest):
            raise ContractError("pinned-file digest is invalid")
        if status != "active" or path in {
            item["path"] for item in result.values()
        }:
            raise ContractError("pinned-file status or path is invalid")
        result[identifier] = entry
    return result


def validate_resource_limits(value: Any) -> None:
    if not isinstance(value, dict):
        raise ContractError("resource limits must be an object")
    exact_keys(
        value,
        {"status", "command_timeout_seconds", "per_log_max_bytes"},
        "resource limits",
    )
    if value != {
        "status": "owner_decision_required",
        "command_timeout_seconds": None,
        "per_log_max_bytes": None,
    }:
        raise ContractError(
            "resource limits require an owner decision and implementation"
        )


def safe_relative_path(value: str) -> bool:
    path = Path(value)
    return (
        bool(value)
        and not path.is_absolute()
        and ".." not in path.parts
        and path.as_posix() == value
    )


def verify_pinned_files(root: Path, pins: dict[str, dict[str, str]]) -> None:
    for entry in pins.values():
        path = root / entry["path"]
        if (
            path.is_symlink()
            or not path.is_file()
            or sha256_file(path) != entry["sha256"]
        ):
            raise ContractError(f"pinned gate file does not match: {entry['id']}")


def validate_descriptor(
    descriptor: dict[str, Any],
    manifest: dict[str, Any],
    run_path: Path,
    expected: dict[str, Any],
) -> None:
    for field in (
        "candidate",
        "base",
        "gate",
        "interpreter",
        "environment",
        "tools",
        "repository_start",
        "repository_end",
    ):
        if descriptor.get(field) != expected[field]:
            raise ContractError(
                f"descriptor {field} differs from the final observed value"
            )
    if descriptor.get("resource_limits") != manifest["resource_limits"]:
        raise ContractError("descriptor resource limits differ from the manifest")
    start = descriptor["repository_start"]
    candidate = descriptor["candidate"]
    if candidate != {"commit": start["commit"], "tree": start["tree"]}:
        raise ContractError("candidate identity differs from the start snapshot")
    validate_tool_records(descriptor["tools"])
    validate_command_records(descriptor, manifest, run_path)
    if descriptor["outcome"] == "passed":
        if descriptor["failure_codes"]:
            raise ContractError("passed descriptor has failure codes")
        if (
            start != descriptor["repository_end"]
            or not start["clean"]
            or not start["submodules_clean"]
        ):
            raise ContractError(
                "passed descriptor does not bind an unchanged clean repository"
            )
        if len(descriptor["commands"]) != len(manifest["commands"]):
            raise ContractError("passed descriptor does not contain every command")
        if any(record["outcome"] != "passed" for record in descriptor["commands"]):
            raise ContractError("passed descriptor contains a failed command")
    elif not descriptor["failure_codes"]:
        raise ContractError("failed descriptor has no failure code")


def validate_tool_records(tools: Any) -> None:
    if not isinstance(tools, list) or not tools:
        raise ContractError("descriptor tools must be a non-empty array")
    identifiers: list[str] = []
    for record in tools:
        if not isinstance(record, dict):
            raise ContractError("tool record must be an object")
        exact_keys(record, {"id", "sha256"}, "tool record")
        if not SAFE_ID.fullmatch(record["id"]) or not HEX_64.fullmatch(
            record["sha256"]
        ):
            raise ContractError("tool record identity is invalid")
        identifiers.append(record["id"])
    if identifiers != sorted(set(identifiers)):
        raise ContractError("tool records must have unique sorted IDs")


def validate_command_records(
    descriptor: dict[str, Any], manifest: dict[str, Any], run_path: Path
) -> None:
    records = descriptor["commands"]
    if not isinstance(records, list) or len(records) > len(manifest["commands"]):
        raise ContractError("descriptor command count is invalid")
    tools = {record["id"]: record for record in descriptor["tools"]}
    referenced: set[str] = set()
    for order, (record, command) in enumerate(
        zip(records, manifest["commands"], strict=False), start=1
    ):
        if record["order"] != order:
            raise ContractError("descriptor command order is not contiguous")
        for field in ("id", "kind", "argv"):
            if record[field] != command[field]:
                raise ContractError("descriptor command differs from the manifest")
        expected_tool = tools.get(tool_id(command["argv"][0]))
        if record["tool"] != expected_tool:
            raise ContractError("descriptor command tool identity is invalid")
        validate_process_relation(record)
        validate_distribution_relation(record)
        for stream in ("stdout", "stderr"):
            expected_name = f"{order:02d}-{command['id']}.{stream}.log"
            validate_log(run_path, record[stream], expected_name, referenced)
    actual = {path.name for path in run_path.iterdir() if path.name.endswith(".log")}
    if referenced != actual:
        raise ContractError("retained logs do not exactly match descriptor references")


def validate_process_relation(record: dict[str, Any]) -> None:
    process = record["process_outcome"]
    exit_code = record["exit_code"]
    if process == "passed" and exit_code != 0:
        raise ContractError("passed process does not have exit zero")
    if process == "failed" and (type(exit_code) is not int or exit_code == 0):
        raise ContractError("failed process does not have a nonzero exit")
    if process == "failed_to_start" and exit_code is not None:
        raise ContractError("unstarted process has an exit code")
    expected_failure = {
        "failed": "command_exit_nonzero",
        "failed_to_start": "command_failed_to_start",
    }.get(process)
    if expected_failure is not None and record["failure_code"] != expected_failure:
        raise ContractError("failed process has the wrong failure classification")
    if (
        process == "passed"
        and record["kind"] != "distribution"
        and record["failure_code"] is not None
    ):
        raise ContractError("successful non-distribution process has a failure code")
    if record["outcome"] == "passed" and (
        process != "passed" or record["failure_code"] is not None
    ):
        raise ContractError("passed command has inconsistent process evidence")
    if record["outcome"] == "failed" and record["failure_code"] is None:
        raise ContractError("failed command has no failure code")


def validate_distribution_relation(record: dict[str, Any]) -> None:
    distribution = record["distribution"]
    if record["kind"] != "distribution":
        if distribution is not None:
            raise ContractError("non-distribution command records a distribution")
        return
    if distribution is None:
        if record["outcome"] == "passed":
            raise ContractError("passed distribution has no counts")
        return
    observations = distribution["observations"]
    passed = distribution["passed"]
    failed = distribution["failed"]
    if observations != passed + failed:
        raise ContractError("distribution counts do not reconcile")
    valid_success = observations >= 20 and failed == 0 and passed == observations
    if record["outcome"] == "passed" and not valid_success:
        raise ContractError("distribution success requirements are not met")
    if (
        record["outcome"] == "failed"
        and record["failure_code"] == "distribution_requirements_failed"
        and valid_success
    ):
        raise ContractError("valid distribution is marked as structurally failed")


def validate_log(
    run_path: Path, record: Any, expected_name: str, referenced: set[str]
) -> None:
    if not isinstance(record, dict):
        raise ContractError("log record must be an object")
    exact_keys(record, {"path", "bytes", "sha256"}, "log record")
    if record["path"] != expected_name or expected_name in referenced:
        raise ContractError("log reference is duplicated or has the wrong path")
    path = run_path / expected_name
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or stat.S_IMODE(info.st_mode) & 0o077:
        raise ContractError("log is not a private regular file")
    if type(record["bytes"]) is not int or record["bytes"] != info.st_size:
        raise ContractError("log byte count differs from retained bytes")
    if not isinstance(record["sha256"], str) or record["sha256"] != sha256_file(path):
        raise ContractError("log digest differs from retained bytes")
    referenced.add(expected_name)
