#!/usr/bin/env python3
"""Generate source-bound contention and durability evidence for P5 D3."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import time
from typing import Final


BRANCH: Final = "codex/p5-d3-compaction"
TOOLCHAIN: Final = "1.94.0"
RUNS: Final = 20
EVIDENCE_DIRECTORY: Final = Path("docs/reviews/evidence/p5-d3")
DISTRIBUTION_TESTS: Final = (
    "session::response_publication_batch_tests::independent_handles_publish_only_contiguous_response_groups",
    "session::response_publication_batch_tests::independent_processes_publish_only_contiguous_response_groups",
)
SENTINEL_TESTS: Final = (
    "tools::agent::fork_seed::tests::persistent_fork_seeds_framed_response_group_without_splitting_it",
    "r#loop::runner::tests::response_publication_timeout::timeout_in_response_event_hook_never_duplicates_durable_output_as_partial",
)
TEST_RESULT = re.compile(
    rb"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; "
    rb"(\d+) ignored; (\d+) measured; (\d+) filtered out"
)
FULL_OBJECT = re.compile(r"[0-9a-f]{40}")
EXPECTED_TEST_COUNTS: Final = {
    "passed": 1,
    "failed": 0,
    "ignored": 0,
    "measured": 0,
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True, help="exact base commit")
    parser.add_argument("--source", required=True, help="exact candidate commit")
    parser.add_argument("--source-tree", required=True, help="exact candidate tree")
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


def exact_commit(repo: Path, revision: str) -> str:
    return git(repo, "rev-parse", f"{revision}^{{commit}}").decode().strip()


def nul_records(raw: bytes) -> list[bytes]:
    if not raw:
        return []
    if not raw.endswith(b"\0"):
        raise RuntimeError("NUL-delimited evidence input is truncated")
    return raw[:-1].split(b"\0")


def nul_paths(raw: bytes) -> list[str]:
    return [record.decode() for record in nul_records(raw)]


def validate_output(repo: Path, output: Path) -> Path:
    resolved = (repo / output).resolve() if not output.is_absolute() else output.resolve()
    allowed = (repo / EVIDENCE_DIRECTORY).resolve()
    if resolved.parent != allowed or resolved.suffix != ".json":
        raise RuntimeError(f"output must be a JSON file directly under {EVIDENCE_DIRECTORY}")
    return resolved


def verify_source(
    repo: Path,
    *,
    base: str,
    source: str,
    source_tree: str,
) -> dict[str, object]:
    object_ids = {"base": base, "source": source, "source-tree": source_tree}
    invalid = [name for name, value in object_ids.items() if not FULL_OBJECT.fullmatch(value)]
    if invalid:
        raise RuntimeError(
            "full lowercase 40-character object IDs are required for " + ", ".join(invalid)
        )

    branch = git(repo, "branch", "--show-current").decode().strip()
    if branch != BRANCH:
        raise RuntimeError(f"P5 D3 evidence must run on {BRANCH}, not {branch!r}")
    if exact_commit(repo, base) != base or exact_commit(repo, source) != source:
        raise RuntimeError("base and source must resolve to the supplied exact commits")
    if base == source or git(repo, "merge-base", base, source).decode().strip() != base:
        raise RuntimeError("source must descend from the distinct supplied base")
    if exact_commit(repo, "HEAD") != source:
        raise RuntimeError("HEAD does not match the supplied P5 D3 source commit")

    observed_tree = git(repo, "rev-parse", "HEAD^{tree}").decode().strip()
    if observed_tree != source_tree:
        raise RuntimeError(
            f"source tree mismatch: expected {source_tree}, got {observed_tree}"
        )

    dirty_rust = nul_paths(git(repo, "diff", "--name-only", "-z", "HEAD", "--", "*.rs"))
    untracked_rust = nul_paths(
        git(repo, "ls-files", "--others", "--exclude-standard", "-z", "--", "*.rs")
    )
    if dirty_rust or untracked_rust:
        paths = sorted(set(dirty_rust + untracked_rust))
        raise RuntimeError("dirty Rust invalidates P5 D3 evidence:\n" + "\n".join(paths))

    inventory_raw = git(
        repo,
        "diff",
        "--name-only",
        "--diff-filter=ACDMRTUXB",
        "-z",
        f"{base}..{source}",
        "--",
    )
    inventory = nul_paths(inventory_raw)
    rust_manifest_raw = git(repo, "ls-tree", "-r", "-z", source, "--", "*.rs")

    runner = Path(__file__).resolve()
    runner_path = str(runner.relative_to(repo))
    committed_runner = git(repo, "show", f"{source}:{runner_path}")
    current_runner = runner.read_bytes()
    if current_runner != committed_runner:
        raise RuntimeError("the checked-out evidence runner differs from the source commit")

    return {
        "base": base,
        "branch": branch,
        "source": source,
        "source_tree": source_tree,
        "dirty_rust_paths": [],
        "inventory": {
            "encoding": "NUL-delimited git diff --name-only -z",
            "count": len(inventory),
            "sha256": sha256(inventory_raw),
            "paths": inventory,
        },
        "rust_manifest": {
            "encoding": "NUL-delimited git ls-tree -r -z",
            "records": len(nul_records(rust_manifest_raw)),
            "sha256": sha256(rust_manifest_raw),
        },
        "runner": {
            "path": runner_path,
            "blob": git(repo, "rev-parse", f"{source}:{runner_path}").decode().strip(),
            "sha256": sha256(committed_runner),
        },
    }


def evidence_environment(repo: Path) -> dict[str, str]:
    target = repo / "target"
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
        "NORN_D3_BATCH_PROCESS_CHILD",
        "NORN_D3_BATCH_PROCESS_ROOT",
        "NORN_D3_BATCH_PROCESS_SESSION",
        "NORN_D3_BATCH_PROCESS_LABEL",
        "NORN_D3_BATCH_PROCESS_READY",
        "NORN_D3_BATCH_PROCESS_START",
    ):
        environment.pop(name, None)
    environment["CARGO_TARGET_DIR"] = str(target)
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    return environment


def cargo(*arguments: str) -> list[str]:
    return ["cargo", f"+{TOOLCHAIN}", "--locked", *arguments]


def library_command(test: str) -> list[str]:
    return cargo(
        "test",
        "-p",
        "norn",
        "--lib",
        test,
        "--",
        "--exact",
        "--test-threads=1",
    )


def observed_counts(raw: bytes) -> tuple[dict[str, int] | None, int]:
    matches = TEST_RESULT.findall(raw)
    if len(matches) != 1:
        return None, len(matches)
    values = [int(value) for value in matches[0]]
    names = ("passed", "failed", "ignored", "measured", "filtered_out")
    return dict(zip(names, values, strict=True)), 1


def execute(
    repo: Path,
    environment: dict[str, str],
    command: list[str],
    iteration: int,
) -> dict[str, object]:
    started = time.monotonic()
    completed = run(command, cwd=repo, environment=environment)
    elapsed = round(time.monotonic() - started, 3)
    counts, summary_records = observed_counts(completed.stdout)
    passed = (
        completed.returncode == 0
        and counts is not None
        and all(counts[name] == value for name, value in EXPECTED_TEST_COUNTS.items())
    )
    if not passed:
        tail = completed.stdout.decode(errors="replace").splitlines()[-80:]
        print("\n".join(tail), file=sys.stderr)
    return {
        "iteration": iteration,
        "command": shlex.join(command),
        "duration_seconds": elapsed,
        "exit_status": completed.returncode,
        "summary_records": summary_records,
        "observed_test_counts": counts,
        "output": {
            "bytes": len(completed.stdout),
            "sha256": sha256(completed.stdout),
        },
        "result": "pass" if passed else "fail",
    }


def run_case(
    repo: Path,
    environment: dict[str, str],
    *,
    test: str,
    runs: int,
) -> dict[str, object]:
    command = library_command(test)
    observations = []
    for iteration in range(1, runs + 1):
        print(f"{test}: {iteration}/{runs}", flush=True)
        observations.append(execute(repo, environment, command, iteration))
    passed = sum(observation["result"] == "pass" for observation in observations)
    return {
        "test": test,
        "command": shlex.join(command),
        "expected_test_counts": EXPECTED_TEST_COUNTS,
        "runs": runs,
        "passed": passed,
        "failed": runs - passed,
        "duration_seconds": round(
            sum(observation["duration_seconds"] for observation in observations), 3
        ),
        "observations": observations,
        "result": "pass" if passed == runs else "fail",
    }


def main() -> int:
    args = parse_args()
    repo = repo_root()
    output = validate_output(repo, args.output)
    provenance = verify_source(
        repo,
        base=args.base,
        source=args.source,
        source_tree=args.source_tree,
    )
    environment = evidence_environment(repo)

    distribution_cases = [
        run_case(repo, environment, test=test, runs=RUNS)
        for test in DISTRIBUTION_TESTS
    ]
    sentinel_cases = [
        run_case(repo, environment, test=test, runs=1) for test in SENTINEL_TESTS
    ]
    all_cases = [*distribution_cases, *sentinel_cases]
    total = sum(case["runs"] for case in all_cases)
    passed = sum(case["passed"] for case in all_cases)
    result = {
        "schema_version": 1,
        "kind": "p5_d3_contention_and_durability_evidence",
        "provenance": provenance,
        "environment": {
            "cargo_target_directory": "<repo>/target",
            "cargo": run(
                ["cargo", f"+{TOOLCHAIN}", "--version"],
                cwd=repo,
                environment=environment,
                check=True,
            ).stdout.decode().strip(),
            "rustc": run(
                ["rustc", f"+{TOOLCHAIN}", "--version"],
                cwd=repo,
                environment=environment,
                check=True,
            ).stdout.decode().strip(),
        },
        "totals": {
            "runs": total,
            "passed": passed,
            "failed": total - passed,
            "duration_seconds": round(
                sum(case["duration_seconds"] for case in all_cases), 3
            ),
        },
        "distribution_cases": distribution_cases,
        "sentinel_cases": sentinel_cases,
        "result": "pass" if passed == total else "fail",
        "acceptance": {
            "d3_accepted": False,
            "p5_accepted": False,
            "boundary": "candidate evidence only; independent review remains required",
        },
    }
    output.write_text(
        json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    return 0 if passed == total else 1


if __name__ == "__main__":
    raise SystemExit(main())
