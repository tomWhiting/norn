"""Execution and provenance support for retained P0 evidence."""

from __future__ import annotations

import datetime as dt
import hashlib
import os
import platform
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Final

from p0_evidence_disclosure import string_has_absolute_path
from p0_evidence_io import checked_output, sha256_file
from p0_evidence_io import strict_json_loads as _strict_json_loads
from p0_evidence_paths import STORAGE_LAYOUT
import p0_evidence_toolchain as toolchain_support
from p0_test_output import (
    exact_unittest_count,
    failure_identity_report,
    test_counts,
    test_names,
    test_summary_lines,
)


EXECUTABLE_NAMES: Final = toolchain_support.EXECUTABLE_NAMES
PATH_FREE_ENVIRONMENT_CONTROLS: Final = {
    "CARGO_INCREMENTAL": "0",
    "CARGO_NET_OFFLINE": "true",
    "CARGO_TERM_COLOR": "never",
    "GIT_CONFIG_NOSYSTEM": "1",
    "LANG": "C",
    "LC_ALL": "C",
    "NO_COLOR": "1",
    "TERM": "dumb",
    "TZ": "UTC",
}
SANITIZED_VARIABLE_NAMES: Final = (
    "CARGO_HOME",
    "CARGO_INCREMENTAL",
    "CARGO_NET_OFFLINE",
    "CARGO_TARGET_DIR",
    "CARGO_TERM_COLOR",
    "GIT_CONFIG_NOSYSTEM",
    "HOME",
    "LANG",
    "LC_ALL",
    "NO_COLOR",
    "PATH",
    "RUSTUP_HOME",
    "TERM",
    "TMPDIR",
    "TZ",
)


@dataclass(frozen=True)
class Case:
    case_id: str
    group: str
    command: tuple[str, ...]
    runs: int = 1
    expected_tests: int | None = None
    minimum_tests: int = 0
    expected_test_names: tuple[str, ...] = ()
    expected_tool_tests: int | None = None
    expected_tool_test_modules: tuple[str, ...] = ()
    recorded_command: tuple[str, ...] | None = None


def strict_json_loads(raw: bytes) -> object:
    return _strict_json_loads(raw)


def prepare_fresh_target_dir(target_dir: Path) -> None:
    resolved = target_dir.resolve()
    if resolved.exists():
        if not resolved.is_dir():
            raise RuntimeError("Cargo target path must be a directory")
        if next(resolved.iterdir(), None) is not None:
            raise RuntimeError("P0 evidence requires an empty Cargo target directory")
        return
    resolved.mkdir(parents=True, mode=0o700)


def evidence_environment(target_dir: Path) -> tuple[dict[str, str], int]:
    source = os.environ
    resolved_target = target_dir.resolve()
    sterile_home = resolved_target / "evidence-home"
    temporary_root = resolved_target / "evidence-tmp"
    sterile_home.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary_root.mkdir(parents=True, exist_ok=True, mode=0o700)
    source_home = Path(source.get("HOME", str(Path.home()))).expanduser()
    cargo_home = Path(
        source.get("CARGO_HOME", str(source_home / ".cargo"))
    ).expanduser()
    rustup_home = Path(
        source.get("RUSTUP_HOME", str(source_home / ".rustup"))
    ).expanduser()
    environment = {
        "CARGO_HOME": str(cargo_home.resolve()),
        "CARGO_INCREMENTAL": PATH_FREE_ENVIRONMENT_CONTROLS["CARGO_INCREMENTAL"],
        "CARGO_NET_OFFLINE": PATH_FREE_ENVIRONMENT_CONTROLS["CARGO_NET_OFFLINE"],
        "CARGO_TARGET_DIR": str(resolved_target),
        "CARGO_TERM_COLOR": PATH_FREE_ENVIRONMENT_CONTROLS["CARGO_TERM_COLOR"],
        "GIT_CONFIG_NOSYSTEM": PATH_FREE_ENVIRONMENT_CONTROLS["GIT_CONFIG_NOSYSTEM"],
        "HOME": str(sterile_home),
        "LANG": PATH_FREE_ENVIRONMENT_CONTROLS["LANG"],
        "LC_ALL": PATH_FREE_ENVIRONMENT_CONTROLS["LC_ALL"],
        "NO_COLOR": PATH_FREE_ENVIRONMENT_CONTROLS["NO_COLOR"],
        "PATH": source.get("PATH", os.defpath),
        "RUSTUP_HOME": str(rustup_home.resolve()),
        "TERM": PATH_FREE_ENVIRONMENT_CONTROLS["TERM"],
        "TMPDIR": str(temporary_root),
        "TZ": PATH_FREE_ENVIRONMENT_CONTROLS["TZ"],
    }
    if tuple(sorted(environment)) != SANITIZED_VARIABLE_NAMES:
        raise RuntimeError("sanitized evidence environment inventory changed")
    return environment, len(set(source) - set(environment))


def repository_state(
    root: Path,
    environment: dict[str, str] | None = None,
) -> dict[str, object]:
    status = checked_output(
        root,
        "git",
        "-c",
        "status.showUntrackedFiles=all",
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        "--ignore-submodules=none",
        environment=environment,
    )
    return {
        "head": checked_output(
            root, "git", "rev-parse", "HEAD", environment=environment
        ),
        "worktree_clean": not bool(status),
    }


def environment_fingerprint(
    environment: dict[str, str], toolchain: str
) -> dict[str, object]:
    return toolchain_support.environment_fingerprint(environment, toolchain)


def cargo_config_fingerprints(
    root: Path, environment: dict[str, str]
) -> dict[str, object]:
    cargo_home = environment.get("CARGO_HOME")
    if cargo_home is None and environment.get("HOME") is not None:
        cargo_home = str(Path(environment["HOME"]) / ".cargo")
    candidates = {"repository": root / ".cargo" / "config.toml"}
    if cargo_home is not None:
        candidates["user"] = Path(cargo_home) / "config.toml"
    return {
        label: {
            "present": path.is_file(),
            "sha256": sha256_file(path) if path.is_file() else None,
        }
        for label, path in candidates.items()
    }


def temporary_filesystem(environment: dict[str, str]) -> dict[str, str]:
    temporary = environment.get("TMPDIR")
    if temporary is None:
        raise RuntimeError(
            "the evidence environment must define its repository-local TMPDIR"
        )
    path = Path(temporary).resolve()
    if platform.system() == "Darwin":
        mount_point = (
            checked_output(
                path,
                "/bin/df",
                "-P",
                str(path),
                environment=environment,
            )
            .splitlines()[-1]
            .split()[-1]
        )
        mount_lines = checked_output(
            path, "/sbin/mount", environment=environment
        ).splitlines()
        marker = f" on {mount_point} ("
        matching = [line for line in mount_lines if marker in line]
        if len(matching) != 1:
            raise RuntimeError("could not identify the temporary filesystem mount")
        filesystem = matching[0].split(marker, 1)[1].split(",", 1)[0].rstrip(")")
    elif platform.system() == "Linux":
        filesystem = checked_output(
            path,
            "/usr/bin/stat",
            "-f",
            "-c",
            "%T",
            str(path),
            environment=environment,
        )
    else:
        filesystem = "unrecorded"
    return {"filesystem": filesystem}


def metadata(
    root: Path,
    environment: dict[str, str],
    removed_environment_count: int,
    mode: str,
    base: str,
    toolchain: str,
) -> dict[str, object]:
    state = repository_state(root, environment)
    if state["worktree_clean"] is not True:
        raise RuntimeError("P0 evidence requires a clean worktree")
    subprocess.run(
        ("git", "merge-base", "--is-ancestor", base, state["head"]),
        cwd=root,
        check=True,
        env=environment,
    )
    versions = toolchain_support.pinned_versions(root, environment, toolchain)
    return {
        "schema_version": 3,
        "kind": mode,
        "generated_at_utc": dt.datetime.now(dt.UTC).isoformat(),
        "base": base,
        "base_commit": checked_output(
            root, "git", "rev-parse", base, environment=environment
        ),
        **state,
        "platform_system": platform.system(),
        "platform": platform.platform(),
        "python": platform.python_version(),
        "rustc": versions["rustc"],
        "cargo": versions["cargo"],
        "environment": {
            **PATH_FREE_ENVIRONMENT_CONTROLS,
            "sanitized_variable_names": sorted(environment),
            "removed_ambient_variable_count": removed_environment_count,
        },
        "environment_fingerprint": environment_fingerprint(environment, toolchain),
        "cargo_config": cargo_config_fingerprints(root, environment),
        "temporary_filesystem": temporary_filesystem(environment),
        "logical_cpu_count": os.cpu_count(),
        "storage_layout": STORAGE_LAYOUT,
    }


def require_macos_apfs_distribution_host(evidence: dict[str, object]) -> None:
    temporary = evidence.get("temporary_filesystem")
    filesystem = temporary.get("filesystem") if isinstance(temporary, dict) else None
    if evidence.get("platform_system") != "Darwin" or filesystem != "apfs":
        raise RuntimeError("P0 macOS race distributions require Darwin on APFS")


def output_digest(output: str) -> dict[str, object]:
    encoded = output.encode()
    return {
        "bytes": len(encoded),
        "sha256": hashlib.sha256(encoded).hexdigest(),
    }


def case_contract_command(case: Case) -> list[str]:
    """Return the explicitly path-free command retained in evidence."""
    command = list(case.recorded_command or case.command)
    if any(argument_has_absolute_path(argument) for argument in command):
        raise RuntimeError(
            f"case {case.case_id} has an absolute recorded command argument"
        )
    return command


def argument_has_absolute_path(argument: str) -> bool:
    return string_has_absolute_path(argument)


def run_once(root: Path, environment: dict[str, str], case: Case) -> dict[str, object]:
    started = time.monotonic()
    result = subprocess.run(
        case.command,
        cwd=root,
        env=environment,
        capture_output=True,
        text=True,
    )
    record: dict[str, object] = {
        "exit_code": result.returncode,
        "duration_ms": round((time.monotonic() - started) * 1000),
        "stdout": output_digest(result.stdout),
        "stderr": output_digest(result.stderr),
    }
    summaries = test_summary_lines(result.stdout, result.stderr)
    counts = test_counts(summaries)
    names = test_names(result.stdout, result.stderr)
    tool_test_count = exact_unittest_count(result.stdout, result.stderr)
    failure_report = failure_identity_report(result.stdout, result.stderr)
    if summaries:
        record["test_summaries"] = summaries
        record["test_counts"] = counts
    if names:
        record["test_names"] = names
    if case.expected_tool_tests is not None:
        record["tool_test_count"] = tool_test_count
    if counts["failed"] > 0 or failure_report.groups:
        record["failed_test_names"] = list(failure_report.names)
        record["failed_test_identity_groups"] = [
            group.as_record() for group in failure_report.groups
        ]
        record["failed_test_identity_complete"] = (
            failure_report.complete
            and bool(summaries)
            and len(failure_report.names) == counts["failed"]
        )
    contract_failure: str | None = None
    if case.expected_tests is not None and counts != {
        "passed": case.expected_tests,
        "failed": 0,
        "ignored": 0,
    }:
        contract_failure = "exact_test_count"
    elif case.minimum_tests > 0 and (
        counts["passed"] < case.minimum_tests
        or counts["failed"] != 0
        or counts["ignored"] != 0
    ):
        contract_failure = "minimum_test_count"
    if case.expected_test_names and tuple(sorted(names)) != tuple(
        sorted(case.expected_test_names)
    ):
        contract_failure = "exact_test_identity"
    if (
        case.expected_tool_tests is not None
        and tool_test_count != case.expected_tool_tests
    ):
        contract_failure = "exact_tool_test_count"
    if failure_report.groups and counts["failed"] == 0 and contract_failure is None:
        contract_failure = "unexpected_failure_identity"
    if contract_failure is not None:
        record["contract_failure"] = contract_failure
    record["passed"] = result.returncode == 0 and contract_failure is None
    if not record["passed"]:
        sys.stderr.write(result.stdout)
        sys.stderr.write(result.stderr)
        if contract_failure is not None:
            sys.stderr.write(f"evidence contract failure: {contract_failure}\n")
    return record


def run_cases(
    root: Path,
    environment: dict[str, str],
    cases: list[Case],
) -> list[dict[str, object]]:
    records = []
    for case in cases:
        runs = [run_once(root, environment, case) for _ in range(case.runs)]
        records.append(
            {
                "id": case.case_id,
                "group": case.group,
                "command": case_contract_command(case),
                "runs": case.runs,
                "expected_tests": case.expected_tests,
                "minimum_tests": case.minimum_tests,
                "expected_test_names": list(case.expected_test_names),
                "expected_tool_tests": case.expected_tool_tests,
                "expected_tool_test_modules": list(case.expected_tool_test_modules),
                "passed": sum(bool(run["passed"]) for run in runs),
                "failed": sum(not bool(run["passed"]) for run in runs),
                "observations": runs,
            }
        )
    return records
