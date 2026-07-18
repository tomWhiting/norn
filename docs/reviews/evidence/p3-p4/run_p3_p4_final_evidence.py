#!/usr/bin/env python3
"""Run and attest the source-bound final P3/P4 evidence."""

from __future__ import annotations

import argparse
import hashlib
import importlib
import json
import os
import shutil
import subprocess
import sys
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Final

sys.dont_write_bytecode = True

CONTRACT_PATH: Final = "docs/reviews/evidence/p3-p4/p3_p4_final_contract.json"
SUPPORT_PATH: Final = "docs/reviews/evidence/p3-p4/p3_p4_final_support.py"
RUNNER: Final = "docs/reviews/evidence/p3-p4/run_p3_p4_final_evidence.py"
POLICY_RUNNER: Final = "docs/reviews/evidence/run_p0_policy_evidence.py"
BASE = TOOLCHAIN = ""
REUSED_ARTIFACTS: dict[str, str] = {}
DIST_TESTS: tuple[str, ...] = ()
commit = command_text = display_path = distribution_records_valid = None
environment_record = execute_case = file_sha256 = gate_records_valid = git = None
manifest = path_inventory = run = sha256 = target_root = write_json = None
source_support_manifest = verify_reused_artifacts = None
p0_support: Any = None


def repo_root() -> Path:
    return Path(__file__).resolve().parents[4]


def bootstrap(source_arg: str) -> None:
    repo = repo_root()

    def raw_git(*args: str) -> bytes:
        return subprocess.run(
            ["git", *args], cwd=repo, check=True, stdout=subprocess.PIPE
        ).stdout

    source = raw_git("rev-parse", f"{source_arg}^{{commit}}").decode().strip()
    head = raw_git("rev-parse", "HEAD^{commit}").decode().strip()
    if source != head:
        raise RuntimeError("final evidence source must be the checked-out HEAD")
    paths = [CONTRACT_PATH, SUPPORT_PATH, RUNNER, POLICY_RUNNER]
    policy_paths = (
        raw_git("ls-tree", "-r", "--name-only", source, "--", "docs/reviews/evidence")
        .decode()
        .splitlines()
    )
    paths.extend(
        path
        for path in policy_paths
        if (Path(path).name.startswith("p0_") and path.endswith(".py"))
        or (Path(path).name.startswith("p0-") and path.endswith(".yml"))
    )
    for path in set(paths):
        expected = raw_git("show", f"{source}:{path}")
        actual = (repo / path).read_bytes()
        if hashlib.sha256(actual).digest() != hashlib.sha256(expected).digest():
            raise RuntimeError(
                f"working evidence bootstrap differs from source: {path}"
            )

    sys.path[:0] = [
        str(Path(__file__).resolve().parent),
        str((repo / "docs/reviews/evidence").resolve()),
    ]
    support = importlib.import_module("p3_p4_final_support")
    policy_support = importlib.import_module("p0_evidence_support")
    contract = json.loads((repo / CONTRACT_PATH).read_text(encoding="utf-8"))
    expected_keys = {
        "schema_version",
        "base",
        "toolchain",
        "distribution_tests",
        "reused_artifacts",
    }
    if set(contract) != expected_keys or contract.get("schema_version") != 1:
        raise RuntimeError("final evidence contract shape changed")
    if (
        len(contract["distribution_tests"]) != 3
        or len(contract["reused_artifacts"]) != 9
    ):
        raise RuntimeError("final evidence contract inventory changed")
    global BASE, TOOLCHAIN, REUSED_ARTIFACTS, DIST_TESTS, p0_support
    BASE = contract["base"]
    TOOLCHAIN = contract["toolchain"]
    REUSED_ARTIFACTS = contract["reused_artifacts"]
    DIST_TESTS = tuple(contract["distribution_tests"])
    p0_support = policy_support
    for name in (
        "commit",
        "command_text",
        "display_path",
        "distribution_records_valid",
        "environment_record",
        "execute_case",
        "file_sha256",
        "gate_records_valid",
        "git",
        "manifest",
        "path_inventory",
        "run",
        "sha256",
        "source_support_manifest",
        "target_root",
        "verify_reused_artifacts",
        "write_json",
    ):
        globals()[name] = getattr(support, name)


def validate_source(repo: Path, source_arg: str) -> tuple[str, dict[str, Any]]:
    source = commit(repo, source_arg)
    if run(
        ["git", "merge-base", "--is-ancestor", BASE, source], cwd=repo, check=False
    ).returncode:
        raise RuntimeError("P3/P4 base is not an ancestor of the source")
    if source != commit(repo, "HEAD"):
        raise RuntimeError("final evidence source must equal the checked-out HEAD")
    dirty = git(repo, "status", "--porcelain", "--untracked-files=no").decode().strip()
    if dirty:
        raise RuntimeError("tracked working-tree changes would invalidate evidence")
    source_runner = git(repo, "show", f"{source}:{RUNNER}")
    if sha256(source_runner) != file_sha256(repo / RUNNER):
        raise RuntimeError("working runner differs from the source-bound runner")
    provenance = {
        "base": BASE,
        "source": source,
        "tree": git(repo, "rev-parse", f"{source}^{{tree}}").decode().strip(),
        "runner_sha256": sha256(source_runner),
        "changed_paths": path_inventory(repo, BASE, source),
        "rust_manifest": manifest(repo, source),
        "support_manifest": source_support_manifest(
            repo, source, (CONTRACT_PATH, SUPPORT_PATH, RUNNER, POLICY_RUNNER)
        ),
        "reused_artifacts": verify_reused_artifacts(repo, source, REUSED_ARTIFACTS),
    }
    return source, provenance


def environment(target: Path) -> tuple[dict[str, str], int]:
    return p0_support.evidence_environment(target)


def gate_cases(
    source: str, policy_scratch: Path
) -> tuple[tuple[str, list[str], bool], ...]:
    cargo = ["cargo", f"+{TOOLCHAIN}", "--locked"]
    return (
        ("fmt", [*cargo, "fmt", "--all", "--", "--check"], False),
        (
            "clippy",
            [
                *cargo,
                "clippy",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ],
            False,
        ),
        (
            "norn_tests",
            [
                *cargo,
                "test",
                "-p",
                "norn",
                "--tests",
                "--all-features",
                "--no-fail-fast",
            ],
            True,
        ),
        (
            "cli_tests",
            [
                *cargo,
                "test",
                "-p",
                "norn-cli",
                "--tests",
                "--all-features",
                "--no-fail-fast",
            ],
            True,
        ),
        (
            "tui_tests",
            [
                *cargo,
                "test",
                "-p",
                "norn-tui",
                "--tests",
                "--all-features",
                "--no-fail-fast",
            ],
            True,
        ),
        (
            "workspace_tests",
            [
                *cargo,
                "test",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--no-fail-fast",
            ],
            True,
        ),
        (
            "doctests",
            [
                *cargo,
                "test",
                "--workspace",
                "--doc",
                "--all-features",
                "--no-fail-fast",
            ],
            True,
        ),
        ("diff_check", ["git", "diff", "--check", f"{BASE}...{source}"], False),
        (
            "policy",
            [
                sys.executable,
                POLICY_RUNNER,
                "--base",
                BASE,
                "--head",
                source,
                "--output",
                str(policy_scratch),
            ],
            False,
        ),
    )


def gate(args: argparse.Namespace) -> int:
    repo = repo_root()
    source, provenance = validate_source(repo, args.source)
    target = target_root(repo)
    scratch = target / "build" / f"p3-p4-final-gate-{source[:8]}-{os.getpid()}"
    scratch.mkdir(parents=True, exist_ok=False)
    policy_scratch = args.policy_output
    env, removed_environment = environment(target)
    cases = gate_cases(source, policy_scratch)
    try:
        results = [execute_case(repo, scratch, env, *case) for case in cases]
        policy = json.loads(policy_scratch.read_text(encoding="utf-8"))
        policy_matches_source = (
            policy.get("base") == BASE
            and policy.get("head") == source
            and policy.get("policy_passed") is True
        )
        passed = (
            all(result["result"] == "pass" for result in results)
            and policy_matches_source
        )
        evidence = {
            "schema_version": 1,
            "kind": "p3_p4_final_gate",
            "generated_at": datetime.now(UTC).isoformat(),
            "provenance": provenance,
            "environment": environment_record(
                repo, env, removed_environment, TOOLCHAIN, p0_support
            ),
            "checks": results,
            "policy": {
                "path": display_path(repo, args.policy_output),
                "sha256": file_sha256(args.policy_output),
            },
            "passed": passed,
        }
        write_json(args.output, evidence)
        return 0 if passed else 1
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def distributions(args: argparse.Namespace) -> int:
    if args.runs != 20:
        raise RuntimeError("final sensitive distributions require exactly 20 runs")
    repo = repo_root()
    source, provenance = validate_source(repo, args.source)
    target = target_root(repo)
    scratch = target / "build" / f"p3-p4-final-distributions-{source[:8]}-{os.getpid()}"
    scratch.mkdir(parents=True, exist_ok=False)
    env, removed_environment = environment(target)
    results = []
    try:
        for test in DIST_TESTS:
            observations = []
            command = distribution_command(test)
            for index in range(1, args.runs + 1):
                record = execute_case(
                    repo,
                    scratch,
                    env,
                    f"{test.rsplit('::', 1)[-1]}-{index}",
                    command,
                    True,
                    expected_tests=1,
                )
                record["iteration"] = index
                observations.append(record)
            results.append(
                {
                    "test": test,
                    "runs": args.runs,
                    "observations": observations,
                    "passed": all(item["result"] == "pass" for item in observations),
                }
            )
        passed = all(case["passed"] for case in results)
        write_json(
            args.output,
            {
                "schema_version": 1,
                "kind": "p3_p4_final_distributions",
                "generated_at": datetime.now(UTC).isoformat(),
                "provenance": provenance,
                "environment": environment_record(
                    repo, env, removed_environment, TOOLCHAIN, p0_support
                ),
                "cases": results,
                "observations": sum(case["runs"] for case in results),
                "passed": passed,
            },
        )
        return 0 if passed else 1
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def distribution_command(test: str) -> list[str]:
    return [
        "cargo",
        f"+{TOOLCHAIN}",
        "--locked",
        "test",
        "-p",
        "norn",
        "--lib",
        "--all-features",
        test,
        "--",
        "--exact",
        "--nocapture",
    ]


def attest(args: argparse.Namespace) -> int:
    repo = repo_root()
    source, provenance = validate_source(repo, args.source)
    gate_doc = json.loads(args.gate.read_text(encoding="utf-8"))
    policy_doc = json.loads(args.policy.read_text(encoding="utf-8"))
    dist_doc = json.loads(args.distributions.read_text(encoding="utf-8"))
    errors = []
    if (
        gate_doc.get("schema_version") != 1
        or gate_doc.get("kind") != "p3_p4_final_gate"
    ):
        errors.append("gate document shape mismatch")
    if (
        dist_doc.get("schema_version") != 1
        or dist_doc.get("kind") != "p3_p4_final_distributions"
    ):
        errors.append("distribution document shape mismatch")
    for label, document in (("gate", gate_doc), ("distributions", dist_doc)):
        if document.get("provenance") != provenance:
            errors.append(f"{label} provenance mismatch")
        if not document.get("passed"):
            errors.append(f"{label} did not pass")
    expected_gate = [
        (case_id, command_text(repo, command), require_tests)
        for case_id, command, require_tests in gate_cases(source, args.policy)
    ]
    if not gate_records_valid(gate_doc.get("checks"), expected_gate):
        errors.append("gate record inventory or result mismatch")
    if (
        policy_doc.get("schema_version") != 2
        or policy_doc.get("base") != BASE
        or policy_doc.get("head") != source
        or not policy_doc.get("policy_passed")
    ):
        errors.append("policy source or verdict mismatch")
    if gate_doc.get("policy", {}).get("sha256") != file_sha256(args.policy):
        errors.append("gate policy hash mismatch")
    if gate_doc.get("policy", {}).get("path") != display_path(repo, args.policy):
        errors.append("gate policy path mismatch")
    dist_commands = [
        command_text(repo, distribution_command(test)) for test in DIST_TESTS
    ]
    if not distribution_records_valid(dist_doc, DIST_TESTS, dist_commands):
        errors.append("distribution record inventory or result mismatch")
    env, removed_environment = environment(target_root(repo))
    current_environment = environment_record(
        repo, env, removed_environment, TOOLCHAIN, p0_support
    )
    if gate_doc.get("environment") != current_environment:
        errors.append("gate environment differs from attestation environment")
    if dist_doc.get("environment") != current_environment:
        errors.append("distribution environment differs from attestation environment")
    artifacts = {
        "gate": file_sha256(args.gate),
        "policy": file_sha256(args.policy),
        "distributions": file_sha256(args.distributions),
    }
    write_json(
        args.output,
        {
            "schema_version": 1,
            "kind": "p3_p4_final_attestation",
            "generated_at": datetime.now(UTC).isoformat(),
            "source": source,
            "tree": provenance["tree"],
            "artifacts": artifacts,
            "errors": errors,
            "passed": not errors,
        },
    )
    return 0 if not errors else 1


def output_path(value: str) -> Path:
    return Path(value).resolve()


def validate_paths(args: argparse.Namespace) -> None:
    allowed = (repo_root().resolve(), target_root(repo_root()).resolve())
    for name in ("output", "policy_output", "gate", "policy", "distributions"):
        path = getattr(args, name, None)
        if path is not None and not any(
            path == root or root in path.parents for root in allowed
        ):
            raise RuntimeError(f"{name} must stay under the repository or its target")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    subparsers = root.add_subparsers(dest="command", required=True)
    gate_parser = subparsers.add_parser("gate")
    gate_parser.add_argument("--source", required=True)
    gate_parser.add_argument("--output", type=output_path, required=True)
    gate_parser.add_argument("--policy-output", type=output_path, required=True)
    gate_parser.set_defaults(action=gate)
    dist_parser = subparsers.add_parser("distributions")
    dist_parser.add_argument("--source", required=True)
    dist_parser.add_argument("--runs", type=int, default=20)
    dist_parser.add_argument("--output", type=output_path, required=True)
    dist_parser.set_defaults(action=distributions)
    attest_parser = subparsers.add_parser("attest")
    attest_parser.add_argument("--source", required=True)
    attest_parser.add_argument("--gate", type=output_path, required=True)
    attest_parser.add_argument("--policy", type=output_path, required=True)
    attest_parser.add_argument("--distributions", type=output_path, required=True)
    attest_parser.add_argument("--output", type=output_path, required=True)
    attest_parser.set_defaults(action=attest)
    return root


def main() -> int:
    args = parser().parse_args()
    try:
        bootstrap(args.source)
        validate_paths(args)
        return int(args.action(args))
    except (OSError, RuntimeError, subprocess.CalledProcessError, ValueError) as error:
        print(f"p3/p4 evidence failed: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
