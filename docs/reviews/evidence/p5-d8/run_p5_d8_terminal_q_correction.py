#!/usr/bin/env python3
"""Run source-bound repeated evidence for the D8 terminal-Q correction."""

from __future__ import annotations

import argparse
from datetime import UTC, datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import sys
import time


BASE = "531200f372741d96ea0810716267e19a0490f027"
BRANCH = "codex/p5-d8-teardown-q-correction"
TOOLCHAIN = "1.94.0"
RUNS = 20
EVIDENCE_DIRECTORY = Path("docs/reviews/evidence/p5-d8")
PLAN = Path("docs/RESPONSES-API-REMEDIATION-PLAN.md")
HANDOFF = Path("docs/reviews/2026-07-23-p5-d8-terminal-q-correction-handoff.md")
NORN_TESTS = (
    "agent::pending_transition_tests::terminal_send_race_never_loses_a_successful_direct_send",
    "agent::pending_teardown_tests::terminal_fail_before_second_q_preserves_exact_messages_and_fifo",
    "agent::pending_teardown_tests::terminal_fail_after_second_q_preserves_exact_messages_and_fifo",
    "tools::agent::spawn::tests::terminal_mailbox::terminal_queue_failure_downgrades_spawn_and_retains_authority",
    "tools::agent::fork_tool::tests::terminal_mailbox::terminal_queue_failure_downgrades_fork_and_retains_authority",
    "tools::agent::coord::close::recovery_tests::callsite_tests::close_agent_unconditionally_checks_recovery_after_join_for_live_entry",
    "tools::agent::coord::close::recovery_tests::callsite_tests::close_agent_no_handle_terminal_path_refuses_late_parent_recovery",
    "r#loop::delivery_pending_tests::cancellation_after_authoritative_append_cannot_redeliver_pending_message",
)
TUI_TESTS = (
    "agents::status_line::recovery_tests::public_snapshot_keeps_terminal_recovery_visible_and_registered",
)
TEST_RESULT = re.compile(
    rb"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; "
    rb"(\d+) ignored; (\d+) measured; (\d+) filtered out"
)
FULL_OBJECT_ID = re.compile(r"[0-9a-f]{40}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True)
    parser.add_argument("--source-tree", required=True)
    parser.add_argument("--output", required=True, type=Path)
    return parser.parse_args()


def repo_root() -> Path:
    return Path(__file__).resolve().parents[4]


def run(
    command: list[str],
    *,
    cwd: Path,
    environment: dict[str, str] | None = None,
    check: bool = False,
) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        command,
        cwd=cwd,
        env=environment,
        check=check,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )


def git(repo: Path, *arguments: str) -> bytes:
    return run(["git", *arguments], cwd=repo, check=True).stdout


def sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def file_sha256(path: Path) -> str:
    return sha256(path.read_bytes())


def exact_commit(repo: Path, revision: str) -> str:
    return git(repo, "rev-parse", f"{revision}^{{commit}}").decode().strip()


def nul_paths(raw: bytes) -> list[bytes]:
    if not raw:
        return []
    if not raw.endswith(b"\0"):
        raise RuntimeError("NUL-delimited path inventory is truncated")
    return raw[:-1].split(b"\0")


def canonical_inventory(paths: list[bytes]) -> bytes:
    return b"".join(path + b"\0" for path in sorted(set(paths)))


def validate_output(repo: Path, output: Path) -> Path:
    resolved = (repo / output).resolve() if not output.is_absolute() else output.resolve()
    allowed = (repo / EVIDENCE_DIRECTORY).resolve()
    if resolved.parent != allowed or resolved.suffix != ".json":
        raise RuntimeError(f"output must be a JSON file directly under {EVIDENCE_DIRECTORY}")
    return resolved


def verify_source(
    repo: Path,
    source: str,
    source_tree: str,
    output: Path,
) -> dict[str, object]:
    if not FULL_OBJECT_ID.fullmatch(source) or not FULL_OBJECT_ID.fullmatch(source_tree):
        raise RuntimeError("source and source-tree must be full 40-character object IDs")
    branch = git(repo, "branch", "--show-current").decode().strip()
    if branch != BRANCH:
        raise RuntimeError(f"D8 evidence must run on {BRANCH}, not {branch!r}")
    if exact_commit(repo, "HEAD") != source:
        raise RuntimeError("HEAD does not match the requested D8 source commit")
    if exact_commit(repo, BASE) != BASE or source == BASE:
        raise RuntimeError("D8 base/source boundary is invalid")
    if git(repo, "merge-base", BASE, source).decode().strip() != BASE:
        raise RuntimeError("D8 source does not descend from the exact review base")
    observed_tree = git(repo, "rev-parse", "HEAD^{tree}").decode().strip()
    if observed_tree != source_tree:
        raise RuntimeError(f"source tree mismatch: expected {source_tree}, got {observed_tree}")

    runner = Path(__file__).resolve().relative_to(repo)
    allowed_dirty = {
        PLAN.as_posix(),
        HANDOFF.as_posix(),
        runner.as_posix(),
        output.relative_to(repo).as_posix(),
    }
    changed = nul_paths(git(repo, "diff", "--name-only", "-z", "HEAD"))
    untracked = nul_paths(
        git(repo, "ls-files", "--others", "--exclude-standard", "-z")
    )
    dirty = sorted(path.decode() for path in set(changed + untracked))
    unexpected_dirty = [path for path in dirty if path not in allowed_dirty]
    if unexpected_dirty:
        raise RuntimeError(
            "dirty non-evidence paths invalidate D8 evidence:\n"
            + "\n".join(unexpected_dirty)
        )

    inventory = canonical_inventory(
        nul_paths(git(repo, "diff", "--name-only", "-z", BASE, source, "--", "*.rs"))
    )
    paths = [path.decode() for path in nul_paths(inventory)]
    return {
        "base": BASE,
        "branch": branch,
        "source": source,
        "source_tree": source_tree,
        "allowed_dirty_evidence_paths": sorted(set(dirty)),
        "unexpected_dirty_paths": [],
        "rust_inventory": {
            "encoding": "bytewise-sorted NUL-delimited paths",
            "records": len(paths),
            "sha256": sha256(inventory),
            "paths": paths,
        },
    }


def primary_repository_target(repo: Path) -> Path:
    common_dir_raw = git(
        repo,
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ).decode().strip()
    common_dir = Path(common_dir_raw).resolve()
    if common_dir.name != ".git" or not common_dir.is_dir():
        raise RuntimeError(f"unexpected Git common directory: {common_dir}")
    return common_dir.parent / "target"


def evidence_environment(repo: Path) -> tuple[dict[str, str], Path]:
    target = primary_repository_target(repo)
    if target.is_symlink():
        raise RuntimeError("repository target directory must not be a symlink")
    target.mkdir(exist_ok=True)
    environment = os.environ.copy()
    for name in (
        "CARGO_BUILD_TARGET",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTDOCFLAGS",
        "RUSTFLAGS",
    ):
        environment.pop(name, None)
    environment["CARGO_TARGET_DIR"] = str(target)
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    return environment, target


def cargo(*arguments: str) -> list[str]:
    return ["cargo", f"+{TOOLCHAIN}", *arguments]


def observed_tests(raw: bytes) -> int | None:
    matches = TEST_RESULT.findall(raw)
    if not matches:
        return None
    return sum(int(match[0]) + int(match[1]) for match in matches)


def execute(
    repo: Path,
    environment: dict[str, str],
    command: list[str],
    iteration: int,
) -> dict[str, object]:
    started = time.monotonic()
    completed = run(command, cwd=repo, environment=environment)
    elapsed = round(time.monotonic() - started, 3)
    observed = observed_tests(completed.stdout)
    passed = completed.returncode == 0 and observed == 1
    if not passed:
        tail = completed.stdout.decode(errors="replace").splitlines()[-60:]
        print("\n".join(tail), file=sys.stderr)
    return {
        "iteration": iteration,
        "result": "pass" if passed else "fail",
        "exit_status": completed.returncode,
        "observed_tests": observed,
        "elapsed_seconds": elapsed,
        "output_sha256": sha256(completed.stdout),
    }


def run_case(
    repo: Path,
    environment: dict[str, str],
    *,
    package: str,
    test: str,
) -> dict[str, object]:
    command = cargo(
        "test",
        "--locked",
        "--quiet",
        "-p",
        package,
        "--lib",
        test,
        "--",
        "--exact",
        "--test-threads=1",
    )
    observations = []
    for iteration in range(1, RUNS + 1):
        print(f"{package}:{test}: {iteration}/{RUNS}", flush=True)
        observations.append(execute(repo, environment, command, iteration))
    passed = sum(item["result"] == "pass" for item in observations)
    return {
        "package": package,
        "test": test,
        "command": " ".join(command),
        "runs": RUNS,
        "passed": passed,
        "failed": RUNS - passed,
        "observations": observations,
    }


def write_json_atomic(output: Path, value: dict[str, object]) -> None:
    temporary = output.with_suffix(".json.tmp")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    os.replace(temporary, output)


def main() -> int:
    args = parse_args()
    repo = repo_root()
    output = validate_output(repo, args.output)
    provenance = verify_source(repo, args.source, args.source_tree, output)
    environment, target = evidence_environment(repo)
    cases = [
        *(run_case(repo, environment, package="norn", test=test) for test in NORN_TESTS),
        *(run_case(repo, environment, package="norn-tui", test=test) for test in TUI_TESTS),
    ]
    passed = sum(case["passed"] for case in cases)
    total = sum(case["runs"] for case in cases)
    result = {
        "schema_version": 1,
        "kind": "p5_d8_terminal_q_correction_distributions",
        "generated_at": datetime.now(UTC).isoformat(),
        "provenance": provenance,
        "runner": {
            "path": str(Path(__file__).resolve().relative_to(repo)),
            "sha256": file_sha256(Path(__file__)),
        },
        "environment": {
            "cargo_target_directory": str(target),
            "cargo_target_policy": "primary repository target resolved from Git common directory",
            "platform": platform.platform(),
            "rustc": run(
                ["rustc", f"+{TOOLCHAIN}", "--version"],
                cwd=repo,
                environment=environment,
                check=True,
            ).stdout.decode().strip(),
        },
        "distribution_policy": {
            "runs_per_case": RUNS,
            "one_exact_test_required_per_observation": True,
        },
        "totals": {"runs": total, "passed": passed, "failed": total - passed},
        "cases": cases,
        "result": "pass" if passed == total else "fail",
        "boundary": "D8 terminal-Q correction evidence only; same-reviewer confirmation required",
    }
    write_json_atomic(output, result)
    return 0 if passed == total else 1


if __name__ == "__main__":
    raise SystemExit(main())
