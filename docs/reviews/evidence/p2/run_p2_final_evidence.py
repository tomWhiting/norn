#!/usr/bin/env python3
"""Generate the deterministic, non-live P2 Gate C evidence bundle."""

from __future__ import annotations

import argparse
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

HERE = Path(__file__).resolve().parent
EVIDENCE = HERE.parent
P3_EVIDENCE = EVIDENCE / "p3-p4"
CONTRACT_PATH: Final = "docs/reviews/evidence/p2/p2_contract.json"
SUPPORT_PATH: Final = "docs/reviews/evidence/p2/p2_evidence_support.py"
REDACTION_PATH: Final = "docs/reviews/evidence/p2/p2_redaction.py"
RUNNER_PATH: Final = "docs/reviews/evidence/p2/run_p2_final_evidence.py"
LIVE_PATH: Final = "docs/reviews/evidence/p2/run_p2_live_aba.py"
PROBE_PATH: Final = "docs/reviews/evidence/p2/p2_live_refresh_probe.rs"
POLICY_RUNNER: Final = "docs/reviews/evidence/run_p0_policy_evidence.py"
COMMON_REDACTION: Final = "docs/reviews/evidence/p3-p4/p3_p4_redaction.py"
COMMON_REDACTION_TEST: Final = "docs/reviews/evidence/p3-p4/test_p3_p4_redaction.py"

support: Any = None
p2_redaction: Any = None
scanner: Any = None
p0_support: Any = None
contract: dict[str, Any] = {}


def bootstrap() -> None:
    if not (sys.flags.isolated and sys.flags.no_site and sys.flags.dont_write_bytecode):
        raise RuntimeError("P2 evidence requires Python -I -S -B")
    sys.path[:0] = [str(HERE), str(EVIDENCE), str(P3_EVIDENCE)]
    global support, p2_redaction, scanner, p0_support, contract
    support = importlib.import_module("p2_evidence_support")
    p2_redaction = importlib.import_module("p2_redaction")
    scanner = importlib.import_module("p3_p4_redaction")
    p0_support = importlib.import_module("p0_evidence_support")
    repo = support.repo_root()
    package = support.commit(repo, "HEAD")
    support.require_clean_package(repo)
    fixed = (
        CONTRACT_PATH,
        SUPPORT_PATH,
        REDACTION_PATH,
        RUNNER_PATH,
        LIVE_PATH,
        PROBE_PATH,
        POLICY_RUNNER,
        COMMON_REDACTION,
        COMMON_REDACTION_TEST,
    )
    for path in fixed:
        expected = support.git(repo, "show", f"{package}:{path}")
        if support.sha256(expected) != support.file_sha256(repo / path):
            raise RuntimeError(f"working evidence support differs from package: {path}")
    contract = support.load_contract(repo / CONTRACT_PATH)
    for key in ("base", "implementation_source", "integration_anchor"):
        if support.commit(repo, contract[key]) != contract[key]:
            raise RuntimeError(f"P2 contract {key} is not an exact commit")
    if subprocess.run(
        ["git", "merge-base", "--is-ancestor", contract["integration_anchor"], package],
        cwd=repo,
        check=False,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    ).returncode:
        raise RuntimeError("P2 evidence package does not descend from the integration anchor")


def environment() -> tuple[dict[str, str], int]:
    target = support.target_root(support.repo_root())
    env, removed = p0_support.evidence_environment(target)
    toolchain = p0_support.toolchain_support
    cargo = toolchain.pinned_binary(env, contract["toolchain"], "cargo")
    env["PATH"] = f"{cargo.parent}{os.pathsep}{env['PATH']}"
    env["RUSTC"] = str(toolchain.pinned_binary(env, contract["toolchain"], "rustc"))
    env["RUSTDOC"] = str(toolchain.pinned_binary(env, contract["toolchain"], "rustdoc"))
    return env, removed


def provenance() -> dict[str, Any]:
    repo = support.repo_root()
    package = support.commit(repo, "HEAD")
    implementation = contract["implementation_source"]
    fixed = (CONTRACT_PATH, SUPPORT_PATH, REDACTION_PATH, RUNNER_PATH, LIVE_PATH, PROBE_PATH)
    return {
        "base": contract["base"],
        "implementation_source": implementation,
        "implementation_tree": support.git(
            repo, "rev-parse", f"{implementation}^{{tree}}"
        ).decode().strip(),
        "integration_anchor": contract["integration_anchor"],
        "evidence_package": package,
        "evidence_package_tree": support.git(repo, "rev-parse", f"{package}^{{tree}}").decode().strip(),
        "changed_paths": support.path_inventory(repo, contract["base"], implementation),
        "rust_manifest": support.rust_manifest(repo, implementation),
        "auth_product_paths": support.auth_path_inventory(
            repo,
            contract["base"],
            implementation,
            contract["integration_anchor"],
            package,
        ),
        "evidence_support": [
            {
                "path": path,
                "sha256": support.sha256(support.git(repo, "show", f"{package}:{path}")),
            }
            for path in fixed
        ],
    }


def cargo_executable(env: dict[str, str]) -> str:
    return str(Path(env["RUSTC"]).parent / "cargo")


def exact_test(case: dict[str, Any], cargo: str) -> list[str]:
    command = [cargo, "--locked", "test", "-p", case["package"]]
    if case["target"] == "lib":
        command.append("--lib")
    else:
        command.extend(("--test", case["target"]))
    command.extend((case["test"], "--", "--exact", "--nocapture"))
    return command


def gate_cases(
    cargo: str, policy: Path
) -> list[tuple[str, list[str], Path, bool, int | None]]:
    repo = support.repo_root()
    cases = [
        (case["id"], exact_test(case, cargo), Path("<source>"), True, 1)
        for case in contract["phase_cases"]
    ]
    cases.extend(
        (
            ("fmt", [cargo, "--locked", "fmt", "--all", "--", "--check"], Path("<source>"), False, None),
            (
                "clippy",
                [cargo, "--locked", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"],
                Path("<source>"),
                False,
                None,
            ),
            ("norn_tests", [cargo, "--locked", "test", "-p", "norn", "--tests", "--no-fail-fast"], Path("<source>"), True, None),
            ("cli_tests", [cargo, "--locked", "test", "-p", "norn-cli", "--tests", "--no-fail-fast"], Path("<source>"), True, None),
            ("workspace_tests", [cargo, "--locked", "test", "--workspace", "--all-targets", "--no-fail-fast"], Path("<source>"), True, None),
            ("doctests", [cargo, "--locked", "test", "--workspace", "--doc", "--no-fail-fast"], Path("<source>"), True, None),
            ("redaction_tests", [sys.executable, "-I", "-S", "-B", COMMON_REDACTION_TEST], repo, True, None),
            ("diff_check", ["git", "diff", "--check", f"{contract['base']}...{contract['implementation_source']}"], repo, False, None),
            (
                "policy",
                [sys.executable, "-I", "-S", "-B", POLICY_RUNNER, "--base", contract["base"], "--head", contract["implementation_source"], "--output", str(policy)],
                repo,
                False,
                None,
            ),
        )
    )
    return cases


def run_gate(output: Path, policy: Path) -> int:
    repo = support.repo_root()
    env, removed = environment()
    target = support.target_root(repo)
    scratch = target / "build" / f"p2-gate-logs-{os.getpid()}"
    scratch.mkdir(parents=True, exist_ok=False)
    try:
        with support.source_checkout(repo, contract["implementation_source"], "gate-source") as source:
            records = []
            for case_id, command, cwd, require_tests, expected in gate_cases(
                cargo_executable(env), policy
            ):
                records.append(
                    support.execute_case(
                        repo,
                        source if str(cwd) == "<source>" else cwd,
                        scratch,
                        env,
                        case_id,
                        command,
                        require_tests,
                        expected,
                    )
                )
        policy_doc = json.loads(policy.read_bytes()) if policy.exists() else {}
        passed = all(record["result"] == "pass" for record in records)
        passed = passed and policy_doc.get("policy_passed") is True
        support.write_json(
            output,
            {
                "schema_version": 1,
                "kind": "p2_final_gate",
                "generated_at": datetime.now(UTC).isoformat(),
                "provenance": provenance(),
                "environment": {
                    "target": "<repo>/target",
                    "removed_ambient_variable_count": removed,
                    "fingerprint": p0_support.environment_fingerprint(env, contract["toolchain"]),
                },
                "checks": records,
                "policy_sha256": support.file_sha256(policy) if policy.exists() else None,
                "passed": passed,
            },
        )
        return 0 if passed else 1
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def run_distributions(output: Path) -> int:
    repo = support.repo_root()
    env, removed = environment()
    target = support.target_root(repo)
    scratch = target / "build" / f"p2-distribution-logs-{os.getpid()}"
    scratch.mkdir(parents=True, exist_ok=False)
    cases = []
    try:
        with support.source_checkout(repo, contract["implementation_source"], "distribution-source") as source:
            for test in contract["distribution_tests"]:
                observations = []
                command = exact_test(
                    {"package": "norn", "target": "lib", "test": test},
                    cargo_executable(env),
                )
                for iteration in range(1, contract["distribution_runs"] + 1):
                    record = support.execute_case(
                        repo,
                        source,
                        scratch,
                        env,
                        f"{test.rsplit('::', 1)[-1]}-{iteration}",
                        command,
                        True,
                        1,
                    )
                    record["iteration"] = iteration
                    observations.append(record)
                cases.append(
                    {
                        "test": test,
                        "runs": contract["distribution_runs"],
                        "observations": observations,
                        "passed": all(item["result"] == "pass" for item in observations),
                    }
                )
        passed = all(case["passed"] for case in cases)
        support.write_json(
            output,
            {
                "schema_version": 1,
                "kind": "p2_final_distributions",
                "generated_at": datetime.now(UTC).isoformat(),
                "provenance": provenance(),
                "environment": {
                    "target": "<repo>/target",
                    "removed_ambient_variable_count": removed,
                    "fingerprint": p0_support.environment_fingerprint(env, contract["toolchain"]),
                },
                "cases": cases,
                "observations": sum(case["runs"] for case in cases),
                "passed": passed,
            },
        )
        return 0 if passed else 1
    finally:
        shutil.rmtree(scratch, ignore_errors=True)


def attest(args: argparse.Namespace) -> int:
    artifacts = {
        "gate": args.gate.read_bytes(),
        "policy": args.policy.read_bytes(),
        "distributions": args.distributions.read_bytes(),
        "redaction": args.redaction.read_bytes(),
    }
    docs = {name: p0_support.strict_json_loads(data) for name, data in artifacts.items()}
    errors = []
    expected = {
        "gate": "p2_final_gate",
        "distributions": "p2_final_distributions",
        "redaction": "p2_final_redaction",
    }
    for name, kind in expected.items():
        if docs[name].get("kind") != kind or docs[name].get("passed") is not True:
            errors.append(f"{name} evidence did not pass")
    if docs["policy"].get("policy_passed") is not True:
        errors.append("policy evidence did not pass")
    if (
        docs["policy"].get("base") != contract["base"]
        or docs["policy"].get("head") != contract["implementation_source"]
    ):
        errors.append("policy source binding mismatch")
    expected_gate_ids = [case["id"] for case in contract["phase_cases"]]
    expected_gate_ids.extend(
        (
            "fmt",
            "clippy",
            "norn_tests",
            "cli_tests",
            "workspace_tests",
            "doctests",
            "redaction_tests",
            "diff_check",
            "policy",
        )
    )
    checks = docs["gate"].get("checks")
    if not isinstance(checks, list) or [item.get("id") for item in checks] != expected_gate_ids:
        errors.append("gate inventory mismatch")
    elif any(
        item.get("exit_status") != 0
        or item.get("result") != "pass"
        or item.get("required_tests") is True
        and (not isinstance(item.get("observed_tests"), int) or item["observed_tests"] < 1)
        for item in checks
    ):
        errors.append("gate result mismatch")
    distribution_cases = docs["distributions"].get("cases")
    if docs["distributions"].get("observations") != 180:
        errors.append("distribution denominator mismatch")
    if not isinstance(distribution_cases, list) or len(distribution_cases) != 9:
        errors.append("distribution inventory mismatch")
    else:
        for case, test in zip(
            distribution_cases, contract["distribution_tests"], strict=True
        ):
            observations = case.get("observations")
            if (
                case.get("test") != test
                or case.get("runs") != 20
                or case.get("passed") is not True
                or not isinstance(observations, list)
                or len(observations) != 20
                or any(
                    item.get("iteration") != index
                    or item.get("observed_tests") != 1
                    or item.get("expected_tests") != 1
                    or item.get("exit_status") != 0
                    or item.get("result") != "pass"
                    for index, item in enumerate(observations, 1)
                )
            ):
                errors.append(f"distribution result mismatch: {test}")
    if docs["gate"].get("provenance") != provenance():
        errors.append("gate provenance mismatch")
    if docs["distributions"].get("provenance") != provenance():
        errors.append("distribution provenance mismatch")
    if docs["gate"].get("policy_sha256") != support.sha256(artifacts["policy"]):
        errors.append("gate policy hash mismatch")
    expected_redaction = p2_redaction.build(
        support.repo_root(),
        contract["base"],
        contract["implementation_source"],
        contract["historical_artifacts"],
        {name: artifacts[name] for name in ("gate", "policy", "distributions")},
        scanner,
        support.git,
    )
    if docs["redaction"] != expected_redaction:
        errors.append("redaction inventory mismatch")
    support.write_json(
        args.output,
        {
            "schema_version": 1,
            "kind": "p2_machine_attestation",
            "generated_at": datetime.now(UTC).isoformat(),
            "source": contract["implementation_source"],
            "artifacts": {name: support.sha256(data) for name, data in artifacts.items()},
            "live_aba": "required_not_included",
            "phase_acceptance": False,
            "errors": errors,
            "passed": not errors,
        },
    )
    return 0 if not errors else 1


def final(args: argparse.Namespace) -> int:
    for path in (args.gate, args.policy, args.distributions, args.redaction, args.output):
        if support.target_root(support.repo_root()).resolve() not in path.resolve().parents:
            raise RuntimeError("P2 evidence outputs must stay below the shared repository target")
        if path.exists():
            path.unlink()
    if run_gate(args.gate, args.policy):
        return 1
    if run_distributions(args.distributions):
        return 1
    generated = {
        "gate": args.gate.read_bytes(),
        "policy": args.policy.read_bytes(),
        "distributions": args.distributions.read_bytes(),
    }
    redaction = p2_redaction.build(
        support.repo_root(),
        contract["base"],
        contract["implementation_source"],
        contract["historical_artifacts"],
        generated,
        scanner,
        support.git,
    )
    support.write_json(args.redaction, redaction)
    if not redaction["passed"]:
        return 1
    return attest(args)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("gate", "policy", "distributions", "redaction", "output"):
        parser.add_argument(f"--{name}", required=True, type=Path)
    args = parser.parse_args()
    try:
        bootstrap()
        return final(args)
    except (OSError, RuntimeError, subprocess.CalledProcessError, ValueError) as error:
        print(f"P2 evidence failed: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
