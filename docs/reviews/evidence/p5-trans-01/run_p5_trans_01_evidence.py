#!/usr/bin/env python3
"""Generate source-bound evidence for the isolated P5 TRANS-01 candidate."""

from __future__ import annotations

import argparse
from datetime import UTC, datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
from typing import Final


BASE: Final = "d46bbe2aa7f9556d010b0662d87e002e45304134"
SOURCE: Final = "e448133d285d9cbabc464ca89d1497a55757f4e1"
SOURCE_TREE: Final = "065a1a73ebbe9e2fcfaf38f0dbd85e6cd6c4440f"
TOOLCHAIN: Final = "1.94.0"
RUNS: Final = 20
EXPECTED_PATHS: Final = (
    "crates/norn/src/provider/mod.rs",
    "crates/norn/src/provider/openai/provider.rs",
    "crates/norn/src/provider/openai_compatible/provider.rs",
    "crates/norn/src/provider/owned_stream.rs",
    "crates/norn/src/provider/owned_stream_integration_tests.rs",
    "crates/norn/src/provider/owned_stream_test_support.rs",
    "crates/norn/src/provider/owned_stream_tests.rs",
    "docs/RESPONSES-API-REMEDIATION-PLAN.md",
)
TESTS: Final = (
    "provider::owned_stream::tests::dropping_stream_aborts_a_rate_limiter_wait",
    "provider::owned_stream::tests::dropping_stream_aborts_its_producer_task",
    "provider::owned_stream::tests::dropping_stream_releases_a_blocked_channel_send",
    "provider::owned_stream::tests::dropping_stream_aborts_real_429_backoff",
    "provider::owned_stream_integration_tests::compatible_provider_receiver_drop_closes_socket",
    "provider::owned_stream_integration_tests::receiver_drop_cancels_error_body_drain",
    "provider::owned_stream_integration_tests::receiver_drop_cancels_response_header_wait",
    "provider::owned_stream_integration_tests::receiver_drop_cancels_silent_sse_read",
    "provider::owned_stream_integration_tests::real_loop_cancellation_closes_provider_socket",
    "provider::owned_stream_integration_tests::real_step_timeout_closes_provider_socket",
)
TEST_RESULT = re.compile(
    rb"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; "
    rb"(\d+) ignored; (\d+) measured; (\d+) filtered out"
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--policy-output", required=True, type=Path)
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


def commit(repo: Path, value: str) -> str:
    return git(repo, "rev-parse", f"{value}^{{commit}}").decode().strip()


def sha256(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def file_sha256(path: Path) -> str:
    return sha256(path.read_bytes())


def nul_paths(raw: bytes) -> list[str]:
    if not raw:
        return []
    if not raw.endswith(b"\0"):
        raise RuntimeError("NUL-delimited path inventory is truncated")
    return [item.decode() for item in raw[:-1].split(b"\0")]


def target_root(repo: Path) -> Path:
    common = Path(
        git(repo, "rev-parse", "--path-format=absolute", "--git-common-dir")
        .decode()
        .strip()
    ).resolve()
    target = common.parent / "target"
    target.mkdir(exist_ok=True)
    if target.is_symlink():
        raise RuntimeError("shared repository target must not be a symlink")
    return target


def evidence_environment(repo: Path) -> dict[str, str]:
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
    environment["CARGO_TARGET_DIR"] = str(target_root(repo))
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    return environment


def cargo(*arguments: str) -> list[str]:
    return ["cargo", f"+{TOOLCHAIN}", "--locked", *arguments]


def command_text(repo: Path, command: list[str]) -> str:
    return " ".join(command).replace(str(target_root(repo)), "<repo>/target")


def output_fingerprint(raw: bytes) -> dict[str, int | str]:
    return {"bytes": len(raw), "sha256": sha256(raw)}


def observed_tests(raw: bytes) -> int | None:
    matches = TEST_RESULT.findall(raw)
    return sum(int(match[0]) for match in matches) if matches else None


def execute(
    repo: Path,
    scratch: Path,
    environment: dict[str, str],
    case_id: str,
    command: list[str],
    *,
    expected_tests: int | None = None,
) -> dict[str, object]:
    started_at = datetime.now(UTC).isoformat()
    started = time.monotonic()
    completed = run(command, cwd=repo, environment=environment)
    elapsed = round(time.monotonic() - started, 3)
    observed = observed_tests(completed.stdout)
    passed = completed.returncode == 0 and (
        expected_tests is None or observed == expected_tests
    )
    log = scratch / f"{case_id}.log"
    log.write_bytes(completed.stdout)
    if not passed:
        tail = completed.stdout.decode(errors="replace").splitlines()[-80:]
        print(f"{case_id} failed\n" + "\n".join(tail), file=sys.stderr)
    return {
        "id": case_id,
        "command": command_text(repo, command),
        "started_at": started_at,
        "elapsed_seconds": elapsed,
        "exit_status": completed.returncode,
        "expected_tests": expected_tests,
        "observed_tests": observed,
        "output": output_fingerprint(completed.stdout),
        "result": "pass" if passed else "fail",
    }


def verify_freeze(repo: Path) -> dict[str, object]:
    if commit(repo, BASE) != BASE or commit(repo, SOURCE) != SOURCE:
        raise RuntimeError("TRANS-01 base or source is not an exact commit")
    tree = git(repo, "rev-parse", f"{SOURCE}^{{tree}}").decode().strip()
    if tree != SOURCE_TREE:
        raise RuntimeError(f"TRANS-01 source tree changed: {tree}")
    if run(
        ["git", "merge-base", "--is-ancestor", SOURCE, "HEAD"], cwd=repo
    ).returncode:
        raise RuntimeError("evidence checkout does not descend from TRANS-01 source")

    later_rust_raw = git(
        repo, "diff", "--name-only", "-z", SOURCE, "HEAD", "--", "*.rs"
    )
    later_rust = nul_paths(later_rust_raw)
    invalid_later_rust = [
        path for path in later_rust if not path.startswith("docs/reviews/evidence/")
    ]
    dirty_rust = b"".join(
        (
            git(repo, "diff", "--name-only", "-z", "HEAD", "--", "*.rs"),
            git(
                repo,
                "ls-files",
                "--others",
                "--exclude-standard",
                "-z",
                "--",
                "*.rs",
            ),
        )
    )
    if invalid_later_rust:
        raise RuntimeError(
            "product Rust changed after TRANS-01 source:\n"
            + "\n".join(invalid_later_rust)
        )
    if dirty_rust:
        raise RuntimeError("dirty Rust would invalidate TRANS-01 evidence")

    inventory_raw = git(
        repo,
        "diff",
        "--name-only",
        "--diff-filter=ACDMRTUXB",
        "-z",
        f"{BASE}..{SOURCE}",
        "--",
    )
    inventory = nul_paths(inventory_raw)
    if tuple(inventory) != EXPECTED_PATHS:
        raise RuntimeError(f"TRANS-01 path inventory changed: {inventory}")
    rust_paths = [path for path in inventory if path.endswith(".rs")]
    rust_blobs = [
        {
            "path": path,
            "blob": git(repo, "rev-parse", f"{SOURCE}:{path}").decode().strip(),
        }
        for path in rust_paths
    ]
    for entry in rust_blobs:
        package_blob = git(
            repo, "rev-parse", f"HEAD:{entry['path']}"
        ).decode().strip()
        if package_blob != entry["blob"]:
            raise RuntimeError(f"TRANS-01 Rust blob drifted: {entry['path']}")
    return {
        "base": BASE,
        "source": SOURCE,
        "source_tree": SOURCE_TREE,
        "package_head": commit(repo, "HEAD"),
        "package_tree": git(repo, "rev-parse", "HEAD^{tree}").decode().strip(),
        "later_evidence_rust_paths": later_rust,
        "later_product_rust_paths": [],
        "dirty_rust_paths": [],
        "inventory": {
            "encoding": "NUL-delimited git diff --name-only -z",
            "count": len(inventory),
            "sha256": sha256(inventory_raw),
            "paths": inventory,
        },
        "rust_blobs": rust_blobs,
    }


def strict_gates(
    repo: Path,
    scratch: Path,
    environment: dict[str, str],
    policy_output: Path,
) -> list[dict[str, object]]:
    commands = (
        ("fmt", cargo("fmt", "--all", "--", "--check")),
        (
            "clippy",
            cargo(
                "clippy",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ),
        ),
        ("diff_check", ["git", "diff", "--check", f"{BASE}..{SOURCE}"]),
        (
            "policy",
            [
                sys.executable,
                "-I",
                "-S",
                "-B",
                "docs/reviews/evidence/run_p0_policy_evidence.py",
                "--base",
                BASE,
                "--head",
                SOURCE,
                "--output",
                str(policy_output),
            ],
        ),
    )
    return [
        execute(repo, scratch, environment, case_id, command)
        for case_id, command in commands
    ]


def distributions(
    repo: Path, scratch: Path, environment: dict[str, str]
) -> list[dict[str, object]]:
    cases = []
    for test in TESTS:
        observations = []
        command = cargo("test", "-p", "norn", "--lib", test, "--", "--exact")
        short = test.rsplit("::", 1)[-1]
        for iteration in range(1, RUNS + 1):
            record = execute(
                repo,
                scratch,
                environment,
                f"{short}-{iteration:02d}",
                command,
                expected_tests=1,
            )
            record["iteration"] = iteration
            observations.append(record)
        passed = sum(item["result"] == "pass" for item in observations)
        cases.append(
            {
                "test": test,
                "runs": RUNS,
                "passed": passed,
                "failed": RUNS - passed,
                "result": "pass" if passed == RUNS else "fail",
                "observations": observations,
            }
        )
    return cases


def write_json(path: Path, value: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def main() -> int:
    args = parse_args()
    repo = repo_root()
    provenance = verify_freeze(repo)
    environment = evidence_environment(repo)
    scratch = target_root(repo) / "build" / f"p5-trans-01-evidence-{os.getpid()}"
    policy_scratch = (
        target_root(repo) / "evidence" / f"p5-trans-01-policy-{os.getpid()}.json"
    )
    scratch.mkdir(parents=True, exist_ok=False)
    policy_scratch.parent.mkdir(parents=True, exist_ok=True)
    try:
        gates = strict_gates(repo, scratch, environment, policy_scratch)
        if policy_scratch.exists():
            args.policy_output.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(policy_scratch, args.policy_output)
        repeated = distributions(repo, scratch, environment)
        policy = json.loads(args.policy_output.read_bytes()) if args.policy_output.exists() else {}
        passed_observations = sum(case["passed"] for case in repeated)
        total_observations = len(TESTS) * RUNS
        passed = (
            all(gate["result"] == "pass" for gate in gates)
            and all(case["result"] == "pass" for case in repeated)
            and passed_observations == total_observations
            and policy.get("policy_passed") is True
        )
        runner = Path(__file__)
        result = {
            "schema_version": 1,
            "kind": "p5_trans_01_candidate_evidence",
            "generated_at": datetime.now(UTC).isoformat(),
            "provenance": provenance,
            "runner": {
                "path": str(runner.relative_to(repo)),
                "sha256": file_sha256(runner),
            },
            "environment": {
                "cargo_target_directory": "<repo>/target",
                "rustc": run(
                    ["rustc", f"+{TOOLCHAIN}", "--version"],
                    cwd=repo,
                    environment=environment,
                    check=True,
                ).stdout.decode().strip(),
                "cargo": run(
                    ["cargo", f"+{TOOLCHAIN}", "--version"],
                    cwd=repo,
                    environment=environment,
                    check=True,
                ).stdout.decode().strip(),
            },
            "strict_gates": gates,
            "policy": {
                "path": str(args.policy_output),
                "sha256": file_sha256(args.policy_output),
                "passed": policy.get("policy_passed") is True,
            },
            "distribution": {
                "tests": len(TESTS),
                "runs_per_test": RUNS,
                "total": total_observations,
                "passed": passed_observations,
                "failed": total_observations - passed_observations,
                "cases": repeated,
            },
            "result": "pass" if passed else "fail",
            "acceptance": {
                "trans_01_accepted": False,
                "p5_accepted": False,
                "boundary": "candidate evidence only; independent review remains required",
            },
        }
        write_json(args.output, result)
        return 0 if passed else 1
    finally:
        shutil.rmtree(scratch, ignore_errors=True)
        policy_scratch.unlink(missing_ok=True)


if __name__ == "__main__":
    raise SystemExit(main())
