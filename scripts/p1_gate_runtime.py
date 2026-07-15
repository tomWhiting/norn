"""Command execution and evidence records for the P1 local gate."""

import json
import re
import subprocess
import tempfile
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

TEST_RESULT = re.compile(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed;")


class ExecutionError(Exception):
    """A command produced structurally invalid evidence."""


def utc_now() -> str:
    return datetime.now(UTC).isoformat(timespec="microseconds").replace("+00:00", "Z")


def strict_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ExecutionError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    if set(value) != expected:
        raise ExecutionError(f"{label} has unknown or missing fields")


def test_executions(stdout: bytes, stderr: bytes) -> int:
    text = stdout.decode("utf-8", errors="replace")
    text += stderr.decode("utf-8", errors="replace")
    return sum(
        int(match.group(1)) + int(match.group(2))
        for match in TEST_RESULT.finditer(text)
    )


def distribution_record(stdout: bytes) -> dict[str, int]:
    value = json.loads(stdout, object_pairs_hook=strict_object)
    if not isinstance(value, dict):
        raise ExecutionError("distribution output must be an object")
    exact_keys(
        value,
        {"schema_version", "observations", "passed", "failed"},
        "distribution output",
    )
    if type(value["schema_version"]) is not int or value["schema_version"] != 1:
        raise ExecutionError("distribution schema version is invalid")
    counts = [value[name] for name in ("observations", "passed", "failed")]
    if any(type(count) is not int or count < 0 for count in counts):
        raise ExecutionError("distribution counts are invalid")
    if value["observations"] != value["passed"] + value["failed"]:
        raise ExecutionError("distribution counts do not reconcile")
    return {name: value[name] for name in ("observations", "passed", "failed")}


def distribution_is_success(value: dict[str, int]) -> bool:
    return (
        value["observations"] >= 20
        and value["failed"] == 0
        and value["passed"] == value["observations"]
    )


def execute_command(
    root: Path,
    run_path: Path,
    environment: dict[str, str],
    order: int,
    command: dict[str, Any],
    actual_prefix: list[str],
    tool: dict[str, str],
    evidence: Any,
) -> tuple[dict[str, Any], str | None]:
    identifier = command["id"]
    started = utc_now()
    process_outcome = "failed_to_start"
    exit_code: int | None = None
    with (
        tempfile.TemporaryFile(mode="w+b", dir=run_path) as stdout_file,
        tempfile.TemporaryFile(mode="w+b", dir=run_path) as stderr_file,
    ):
        try:
            result = subprocess.run(
                [*actual_prefix, *command["argv"][1:]],
                cwd=root,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=stdout_file,
                stderr=stderr_file,
                check=False,
            )
            exit_code = result.returncode
            process_outcome = "passed" if exit_code == 0 else "failed"
        except OSError:
            pass
        stdout_file.flush()
        stderr_file.flush()
        stdout_file.seek(0)
        stderr_file.seek(0)
        raw_stdout = stdout_file.read()
        raw_stderr = stderr_file.read()
        executions = test_executions(raw_stdout, raw_stderr)
        distribution: dict[str, int] | None = None
        distribution_failure: str | None = None
        if command["kind"] == "distribution":
            try:
                distribution = distribution_record(raw_stdout)
                if not distribution_is_success(distribution):
                    distribution_failure = "distribution_requirements_failed"
            except (ExecutionError, UnicodeError, json.JSONDecodeError):
                distribution_failure = "invalid_distribution_output"
    failure_code: str | None
    if process_outcome == "failed_to_start":
        failure_code = "command_failed_to_start"
    elif process_outcome == "failed":
        failure_code = "command_exit_nonzero"
    else:
        failure_code = distribution_failure
    outcome = "passed" if failure_code is None else "failed"
    record: dict[str, Any] = {
        "order": order,
        "id": identifier,
        "kind": command["kind"],
        "argv": command["argv"],
        "tool": tool,
        "started_at": started,
        "completed_at": utc_now(),
        "process_outcome": process_outcome,
        "outcome": outcome,
        "exit_code": exit_code,
        "failure_code": failure_code,
        "test_executions": executions,
        "distribution": distribution,
    }
    evidence.write_structured_logs(run_path, record)
    evidence.refresh_record(run_path, record)
    failure = None if failure_code is None else f"{failure_code}:{identifier}"
    return record, failure
