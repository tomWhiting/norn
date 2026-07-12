#!/usr/bin/env python3
"""Run and report the P0 concurrency-sensitive regression distribution."""

from __future__ import annotations

import argparse
import json
import platform
import subprocess
import time
from pathlib import Path


TESTS = (
    "session::manager::tests::open_or_resume_concurrent_same_id_converges_on_one_session",
    "util::private_fs::tests::concurrent_independent_roots_open_one_shared_lock",
    "util::private_fs::tests::concurrent_create_new_has_exactly_one_winner",
)


def positive_integer(value: str) -> int:
    parsed = int(value)
    if parsed < 1:
        raise argparse.ArgumentTypeError("value must be at least 1")
    return parsed


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--runs",
        required=True,
        type=positive_integer,
        help="complete process-isolated invocations per regression",
    )
    return parser.parse_args()


def git_value(root: Path, *args: str) -> str:
    result = subprocess.run(
        ("git", *args),
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def run_test(root: Path, test_name: str) -> dict[str, object]:
    command = (
        "cargo",
        "test",
        "-p",
        "norn",
        test_name,
        "--",
        "--exact",
    )
    started = time.monotonic()
    result = subprocess.run(command, cwd=root, capture_output=True, text=True)
    duration_ms = round((time.monotonic() - started) * 1000)
    record: dict[str, object] = {
        "exit_code": result.returncode,
        "duration_ms": duration_ms,
    }
    if result.returncode != 0:
        record["stdout"] = result.stdout
        record["stderr"] = result.stderr
    return record


def main() -> int:
    args = parse_args()
    root = Path(__file__).resolve().parents[3]
    results: dict[str, list[dict[str, object]]] = {}
    for test_name in TESTS:
        results[test_name] = [run_test(root, test_name) for _ in range(args.runs)]

    summary = {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "head": git_value(root, "rev-parse", "HEAD"),
        "runs_per_test": args.runs,
        "tests": {
            test_name: {
                "passed": sum(record["exit_code"] == 0 for record in records),
                "failed": sum(record["exit_code"] != 0 for record in records),
                "runs": records,
            }
            for test_name, records in results.items()
        },
    }
    # The worktree is intentionally reported as a state, not embedded wholesale.
    summary["worktree_status"] = git_value(root, "status", "--short")
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 1 if any(
        record["exit_code"] != 0
        for records in results.values()
        for record in records
    ) else 0


if __name__ == "__main__":
    raise SystemExit(main())
