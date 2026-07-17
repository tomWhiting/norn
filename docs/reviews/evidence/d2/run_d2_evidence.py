#!/usr/bin/env python3
"""Run provenance-bearing D2 gates and process-isolated distributions."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import platform
import re
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Final


BASE: Final = "2c0350d"
TOOLCHAIN: Final = "1.94.0"
MINIMUM_RUNS: Final = 20
RECOVERY_PREFIX: Final = "D2_RECOVERY_EVENT="
TEST_RESULT = re.compile(
    rb"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; "
    rb"(\d+) ignored; (\d+) measured; (\d+) filtered out"
)
RECOVERY_EVENT = re.compile(rb"(?m)^D2_RECOVERY_EVENT=([a-z0-9_]+)\r?$")


@dataclass(frozen=True)
class DistributionCase:
    name: str
    recovery_event: str | None = None


BASE_DISTRIBUTIONS: Final = (
    DistributionCase(
        "session::manager::tests::"
        "open_or_resume_concurrent_same_id_converges_on_one_session"
    ),
    DistributionCase(
        "session::persistence::timeline_concurrency_tests::"
        "concurrent_registered_sinks_reconcile_exact_timeline_counters"
    ),
    DistributionCase(
        "session::persistence::timeline_concurrency_tests::"
        "concurrent_exact_batch_retries_converge_on_one_event"
    ),
    DistributionCase(
        "session::persistence::timeline_concurrency_tests::"
        "reader_waits_for_inflight_tail_before_recovery"
    ),
    DistributionCase(
        "session::persistence::timeline_concurrency_tests::"
        "delete_waits_for_timeline_owner_and_stale_writers_cannot_recreate"
    ),
    DistributionCase(
        "session::manager::tests::"
        "concurrent_migrated_resumes_converge_on_one_boundary"
    ),
    DistributionCase(
        "session::persistence::timeline_concurrency_tests::"
        "registered_sink_holds_generation_while_waiting_for_timeline"
    ),
    DistributionCase(
        "session::persistence::timeline_runtime_tests::"
        "stale_registered_reader_cannot_repair_recreated_timeline"
    ),
)
RECOVERY_DISTRIBUTIONS: Final = tuple(
    DistributionCase(f"session::migration::tests::migration_recovers_after_{event}", event)
    for event in (
        "backup_prepared",
        "backup_published",
        "backup_durable",
        "strict_store_prepared",
        "strict_store_published",
        "strict_store_durable",
    )
)
ALL_DISTRIBUTIONS: Final = BASE_DISTRIBUTIONS + RECOVERY_DISTRIBUTIONS


def cargo(*arguments: str) -> tuple[str, ...]:
    return ("cargo", f"+{TOOLCHAIN}", "--locked", *arguments)


GATE_CASES: Final = (
    ("format", cargo("fmt", "--all", "--", "--check")),
    (
        "clippy_workspace_all_targets",
        cargo("clippy", "--workspace", "--all-targets", "--", "-D", "warnings"),
    ),
    ("test_workspace_all_targets", cargo("test", "--workspace", "--all-targets")),
    ("test_workspace_doc", cargo("test", "--workspace", "--doc")),
    ("test_norn_persistence", cargo("test", "-p", "norn", "session::persistence")),
    ("test_norn_migration", cargo("test", "-p", "norn", "session::migration")),
    ("test_norn_manager", cargo("test", "-p", "norn", "session::manager")),
    ("test_cli_session", cargo("test", "-p", "norn-cli", "session")),
    ("test_norn_config_paths", cargo("test", "-p", "norn", "config::paths")),
    ("test_cli_config_paths", cargo("test", "-p", "norn-cli", "config::paths")),
)


def minimum_runs(value: str) -> int:
    parsed = int(value)
    if parsed < MINIMUM_RUNS:
        raise argparse.ArgumentTypeError(
            f"distribution runs must be at least {MINIMUM_RUNS}"
        )
    return parsed


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("gate", "distributions", "all"))
    parser.add_argument(
        "--runs", type=minimum_runs, default=MINIMUM_RUNS,
        help="process-isolated invocations per exact distribution test",
    )
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument(
        "--cargo-target-dir",
        type=Path,
        help="shared Cargo target directory; defaults to this checkout's target/",
    )
    return parser.parse_args()


def repository_root() -> Path:
    return Path(__file__).resolve().parents[4]


def sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def output_fingerprint(raw: bytes) -> dict[str, object]:
    return {"bytes": len(raw), "sha256": sha256(raw)}


def capture(root: Path, *command: str, environment: dict[str, str] | None = None) -> bytes:
    return subprocess.run(
        command,
        cwd=root,
        env=environment,
        check=True,
        capture_output=True,
    ).stdout


def git_text(root: Path, *arguments: str) -> str:
    return capture(root, "git", *arguments).decode("utf-8").strip()


def nul_paths(raw: bytes) -> list[str]:
    if not raw:
        return []
    if not raw.endswith(b"\0"):
        raise RuntimeError("NUL-delimited git path inventory was truncated")
    return [path.decode("utf-8") for path in raw[:-1].split(b"\0")]


def repository_state(root: Path) -> dict[str, object]:
    head = git_text(root, "rev-parse", "HEAD")
    status = capture(
        root,
        "git",
        "-c",
        "status.showUntrackedFiles=all",
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
        "--ignore-submodules=none",
    )
    inventory_raw = capture(
        root,
        "git",
        "diff",
        "--name-only",
        "--diff-filter=ACDMRTUXB",
        "-z",
        f"{BASE}..{head}",
        "--",
    )
    inventory = nul_paths(inventory_raw)
    working_raw = capture(root, "git", "diff", "--name-only", "-z", "HEAD", "--")
    untracked_raw = capture(root, "git", "ls-files", "--others", "--exclude-standard", "-z", "--")
    working_paths = sorted(set(nul_paths(working_raw) + nul_paths(untracked_raw)))
    candidate_paths = set(inventory)
    candidate_dirty = [path for path in working_paths if path in candidate_paths]
    return {
        "head": head,
        "head_tree": git_text(root, "rev-parse", "HEAD^{tree}"),
        "worktree_clean": not status,
        "worktree_status": status.decode("utf-8").splitlines(),
        "worktree_status_fingerprint": output_fingerprint(status),
        "candidate_paths_clean": not candidate_dirty,
        "candidate_dirty_paths": candidate_dirty,
        "noncandidate_dirty_paths": [
            path for path in working_paths if path not in candidate_paths
        ],
        "base_diff_name_inventory": {
            "command": [
                "git",
                "diff",
                "--name-only",
                "--diff-filter=ACDMRTUXB",
                "-z",
                f"{BASE}..HEAD",
                "--",
            ],
            "count": len(inventory),
            "paths": inventory,
            "raw_fingerprint": output_fingerprint(inventory_raw),
        },
    }


def evidence_environment(target_dir: Path) -> tuple[dict[str, str], list[str]]:
    environment = os.environ.copy()
    removed = []
    for name in (
        "CARGO_BUILD_TARGET",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_INCREMENTAL",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTC_WRAPPER",
        "RUSTDOCFLAGS",
        "RUSTFLAGS",
    ):
        if environment.pop(name, None) is not None:
            removed.append(name)
    environment.update(
        {
            "CARGO_TARGET_DIR": str(target_dir),
            "CARGO_TERM_COLOR": "never",
            "NO_COLOR": "1",
            "TERM": "dumb",
        }
    )
    return environment, removed


def tool_metadata(root: Path, environment: dict[str, str]) -> dict[str, object]:
    rustc = capture(root, "rustc", f"+{TOOLCHAIN}", "-Vv", environment=environment)
    cargo_version = capture(
        root,
        "cargo",
        f"+{TOOLCHAIN}",
        "--version",
        "--verbose",
        environment=environment,
    )
    return {
        "requested_toolchain": TOOLCHAIN,
        "rustc": rustc.decode("utf-8").strip(),
        "cargo": cargo_version.decode("utf-8").strip(),
    }


def run_command(
    root: Path, environment: dict[str, str], command: tuple[str, ...]
) -> tuple[subprocess.CompletedProcess[bytes], dict[str, object]]:
    started = time.monotonic_ns()
    result = subprocess.run(
        command,
        cwd=root,
        env=environment,
        capture_output=True,
    )
    duration_ms = round((time.monotonic_ns() - started) / 1_000_000, 3)
    record = {
        "command": list(command),
        "exit_code": result.returncode,
        "duration_ms": duration_ms,
        "stdout": output_fingerprint(result.stdout),
        "stderr": output_fingerprint(result.stderr),
    }
    return result, record


def run_gate(root: Path, environment: dict[str, str]) -> dict[str, object]:
    records = []
    for name, command in GATE_CASES:
        result, record = run_command(root, environment, command)
        record["name"] = name
        contract_failures = []
        if name.startswith("test_"):
            counts = parsed_test_counts(result.stdout + b"\n" + result.stderr)
            record["test_counts"] = counts
            if counts["summary_count"] == 0:
                contract_failures.append("test_output_had_no_result_summary")
            if counts["executed"] == 0:
                contract_failures.append("test_command_executed_no_tests")
        record["contract_failures"] = contract_failures
        record["passed"] = result.returncode == 0 and not contract_failures
        records.append(record)
        print(f"gate {name}: {'PASS' if record['passed'] else 'FAIL'}", flush=True)
    passed = sum(bool(record["passed"]) for record in records)
    return {
        "cases": records,
        "passed": passed,
        "failed": len(records) - passed,
        "denominator": len(records),
    }


def parsed_test_counts(raw: bytes) -> dict[str, int]:
    matches = TEST_RESULT.findall(raw)
    totals = [sum(int(value) for value in match[:4]) for match in matches]
    return {
        "summary_count": len(matches),
        "executed": sum(totals),
        "passed": sum(int(match[0]) for match in matches),
        "failed": sum(int(match[1]) for match in matches),
        "ignored": sum(int(match[2]) for match in matches),
        "measured": sum(int(match[3]) for match in matches),
        "filtered_out": sum(int(match[4]) for match in matches),
    }


def recovery_events(raw: bytes) -> list[str]:
    return [event.decode("ascii") for event in RECOVERY_EVENT.findall(raw)]


def run_distribution_once(
    root: Path,
    environment: dict[str, str],
    case: DistributionCase,
) -> dict[str, object]:
    command = cargo(
        "test", "-p", "norn", "--lib", case.name, "--", "--exact", "--nocapture"
    )
    result, record = run_command(root, environment, command)
    combined = result.stdout + b"\n" + result.stderr
    counts = parsed_test_counts(combined)
    events = recovery_events(combined)
    expected_events = [] if case.recovery_event is None else [case.recovery_event]
    contract_failures = []
    if counts["summary_count"] != 1:
        contract_failures.append("expected_one_test_result_summary")
    if counts["executed"] != 1:
        contract_failures.append("exact_filter_did_not_execute_one_test")
    if not (
        counts["passed"] == 1
        and counts["failed"] == 0
        and counts["ignored"] == 0
        and counts["measured"] == 0
    ):
        contract_failures.append("exact_test_did_not_pass_once")
    if events != expected_events:
        contract_failures.append("recovery_event_inventory_mismatch")
    record.update(
        {
            "test_counts": counts,
            "recovery_events": events,
            "expected_recovery_events": expected_events,
            "contract_failures": contract_failures,
            "passed": result.returncode == 0 and not contract_failures,
        }
    )
    return record


def run_distributions(
    root: Path, environment: dict[str, str], runs: int
) -> dict[str, object]:
    cases = []
    for case in ALL_DISTRIBUTIONS:
        observations = []
        for iteration in range(1, runs + 1):
            record = run_distribution_once(root, environment, case)
            record["iteration"] = iteration
            observations.append(record)
        passed = sum(bool(record["passed"]) for record in observations)
        cases.append(
            {
                "test": case.name,
                "expected_recovery_event": case.recovery_event,
                "runs": runs,
                "passed": passed,
                "failed": runs - passed,
                "observations": observations,
            }
        )
        print(f"distribution {case.name}: {passed}/{runs}", flush=True)
    passed = sum(case["passed"] for case in cases)
    denominator = runs * len(cases)
    return {
        "minimum_runs_per_test": MINIMUM_RUNS,
        "runs_per_test": runs,
        "exact_test_count": len(cases),
        "cases": cases,
        "passed": passed,
        "failed": denominator - passed,
        "denominator": denominator,
    }


def validate_output(root: Path, output: Path) -> Path:
    resolved = (root / output).resolve() if not output.is_absolute() else output.resolve()
    evidence_root = (root / "docs" / "reviews" / "evidence" / "d2").resolve()
    if resolved.parent != evidence_root:
        raise RuntimeError("D2 evidence output must be directly inside its evidence directory")
    if resolved == Path(__file__).resolve():
        raise RuntimeError("evidence output cannot replace the runner")
    if resolved.exists():
        raise RuntimeError("evidence output already exists; choose a new immutable filename")
    return resolved


def cargo_target_dir(root: Path, requested: Path | None) -> Path:
    target = root / "target" if requested is None else requested
    if not target.is_absolute():
        target = root / target
    return Path(os.path.abspath(target))


def write_json(output: Path, payload: dict[str, object]) -> None:
    raw = (json.dumps(payload, indent=2, sort_keys=True) + "\n").encode("utf-8")
    temporary = output.with_suffix(output.suffix + ".new")
    temporary.write_bytes(raw)
    os.replace(temporary, output)


def main() -> int:
    args = parse_args()
    root = repository_root()
    output = validate_output(root, args.output)
    target_dir = cargo_target_dir(root, args.cargo_target_dir)
    subprocess.run(("git", "merge-base", "--is-ancestor", BASE, "HEAD"), cwd=root, check=True)
    environment, removed_environment = evidence_environment(target_dir)
    initial_state = repository_state(root)
    if not initial_state["worktree_clean"]:
        raise RuntimeError("D2 evidence requires a fully clean checkout")
    payload: dict[str, object] = {
        "schema_version": 1,
        "kind": "d2_strict_session_store_evidence",
        "mode": args.mode,
        "generated_at_utc": dt.datetime.now(dt.UTC).isoformat(),
        "base": BASE,
        "base_commit": git_text(root, "rev-parse", f"{BASE}^{{commit}}"),
        "base_tree": git_text(root, "rev-parse", f"{BASE}^{{tree}}"),
        "repository": initial_state,
        "runner": {
            "path": str(Path(__file__).resolve().relative_to(root)),
            "sha256": sha256(Path(__file__).read_bytes()),
        },
        "platform": {
            "system": platform.system(),
            "release": platform.release(),
            "version": platform.version(),
            "machine": platform.machine(),
            "platform": platform.platform(),
            "python": platform.python_version(),
        },
        "toolchain": tool_metadata(root, environment),
        "execution_environment": {
            "cargo_target_dir": str(target_dir),
            "cargo_target_dir_resolved": str(target_dir.resolve()),
            "cargo_incremental": "cargo_default",
            "removed_ambient_build_variables": removed_environment,
        },
    }
    if args.mode in ("gate", "all"):
        payload["gate"] = run_gate(root, environment)
    if args.mode in ("distributions", "all"):
        payload["distributions"] = run_distributions(root, environment, args.runs)
    final_state = repository_state(root)
    payload["final_repository"] = final_state
    payload["repository_stable"] = final_state == initial_state
    sections = [payload[key] for key in ("gate", "distributions") if key in payload]
    payload["passed"] = (
        bool(payload["repository_stable"])
        and bool(initial_state["worktree_clean"])
        and bool(final_state["worktree_clean"])
        and bool(initial_state["candidate_paths_clean"])
        and bool(final_state["candidate_paths_clean"])
        and all(
            isinstance(section, dict) and section.get("failed") == 0
            for section in sections
        )
    )
    write_json(output, payload)
    print(f"wrote {output.relative_to(root)}", flush=True)
    return 0 if payload["passed"] else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.SubprocessError, ValueError) as error:
        print(f"D2 evidence runner failed: {error}", file=sys.stderr)
        raise SystemExit(2) from None
