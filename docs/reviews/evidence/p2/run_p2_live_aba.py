#!/usr/bin/env python3
"""Run the owner-approved, credentialed P2 A/B/A refresh experiment."""

from __future__ import annotations

import argparse
import importlib
import os
import re
import shutil
import subprocess
import sys
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
EVIDENCE = HERE.parent
CONTRACT_PATH = "docs/reviews/evidence/p2/p2_contract.json"
PROBE_PATH = "docs/reviews/evidence/p2/p2_live_refresh_probe.rs"
APPROVAL = "I_APPROVE_P2_LIVE_ABA_CREDENTIAL_USE"
ALIAS = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")


def load(require_clean: bool) -> tuple[Any, dict[str, Any]]:
    if not (sys.flags.isolated and sys.flags.no_site and sys.flags.dont_write_bytecode):
        raise RuntimeError("P2 live evidence requires Python -I -S -B")
    sys.path[:0] = [str(HERE), str(EVIDENCE)]
    support = importlib.import_module("p2_evidence_support")
    repo = support.repo_root()
    if require_clean and support.git(repo, "status", "--porcelain", "--untracked-files=all"):
        raise RuntimeError("P2 live evidence requires a completely clean worktree")
    contract = support.load_contract(repo / CONTRACT_PATH)
    if require_clean:
        package = support.commit(repo, "HEAD")
        for path in (CONTRACT_PATH, PROBE_PATH, "docs/reviews/evidence/p2/run_p2_live_aba.py"):
            expected = support.git(repo, "show", f"{package}:{path}")
            if support.sha256(expected) != support.file_sha256(repo / path):
                raise RuntimeError(f"working live-evidence support differs from package: {path}")
        support.auth_path_inventory(
            repo,
            contract["base"],
            contract["implementation_source"],
            contract["integration_anchor"],
            package,
        )
    return support, contract


def required_alias(name: str) -> str:
    value = os.environ.get(name, "")
    if not ALIAS.fullmatch(value):
        raise RuntimeError(f"{name} must contain one valid named-account alias")
    return value


def cargo_build(
    source: Path, target: Path, env: dict[str, str], toolchain: str
) -> tuple[Path, Path]:
    commands = (
        ["cargo", f"+{toolchain}", "build", "--locked", "-p", "norn-cli", "--bin", "norn"],
        ["cargo", f"+{toolchain}", "build", "--locked", "-p", "norn", "--example", "p2_live_refresh_probe"],
    )
    for command in commands:
        completed = subprocess.run(command, cwd=source, env=env, check=False)
        if completed.returncode:
            raise RuntimeError("P2 live harness build failed")
    binaries = target / f"p2-live-binaries-{os.getpid()}"
    binaries.mkdir(exist_ok=False)
    norn = binaries / "norn"
    probe = binaries / "p2_live_refresh_probe"
    shutil.copy2(target / "debug" / "norn", norn)
    shutil.copy2(target / "debug" / "examples" / "p2_live_refresh_probe", probe)
    return norn, probe


def login(binary: Path, alias: str, env: dict[str, str]) -> bool:
    completed = subprocess.run(
        [str(binary), "auth", "login", "--name", alias],
        env=env,
        check=False,
    )
    return completed.returncode == 0


def probe(binary: Path, alias: str, env: dict[str, str]) -> bool:
    child_env = dict(env)
    child_env["NORN_P2_LIVE_ACCOUNT_ALIAS"] = alias
    completed = subprocess.run(
        [str(binary)],
        env=child_env,
        check=False,
        stdout=subprocess.PIPE,
        stderr=None,
    )
    return completed.returncode == 0 and completed.stdout.strip() == b"P2_LIVE_REFRESH_PROBE_PASS"


def execute(args: argparse.Namespace, support: Any, contract: dict[str, Any]) -> int:
    if os.environ.get("NORN_P2_LIVE_APPROVAL") != APPROVAL:
        raise RuntimeError("explicit P2 live credential approval is absent")
    account_a = required_alias("NORN_P2_LIVE_ACCOUNT_A")
    account_b = required_alias("NORN_P2_LIVE_ACCOUNT_B")
    if account_a.casefold() == account_b.casefold():
        raise RuntimeError("P2 live aliases must be distinct")
    repo = support.repo_root()
    target = support.target_root(repo)
    if target.resolve() not in args.output.resolve().parents:
        raise RuntimeError("P2 live output must stay below the shared repository target")
    if args.output.exists():
        raise RuntimeError("P2 live output already exists")
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(target)
    package = support.commit(repo, "HEAD")
    steps = []
    passed = False
    with support.source_checkout(repo, contract["implementation_source"], "live-source") as source:
        probe_source = source / "crates/norn/examples/p2_live_refresh_probe.rs"
        shutil.copy2(repo / PROBE_PATH, probe_source)
        norn, refresh_probe = cargo_build(source, target, env, contract["toolchain"])
        try:
            sequence = (
                ("login_a", lambda: login(norn, account_a, env)),
                ("refresh_a_before_b", lambda: probe(refresh_probe, account_a, env)),
                ("login_b", lambda: login(norn, account_b, env)),
                ("refresh_b", lambda: probe(refresh_probe, account_b, env)),
                ("refresh_a_after_b", lambda: probe(refresh_probe, account_a, env)),
            )
            for label, operation in sequence:
                succeeded = operation()
                steps.append({"step": label, "passed": succeeded})
                if not succeeded:
                    break
            passed = len(steps) == len(sequence) and all(step["passed"] for step in steps)
        finally:
            shutil.rmtree(norn.parent, ignore_errors=True)
    support.write_json(
        args.output,
        {
            "schema_version": 1,
            "kind": "p2_live_aba",
            "generated_at": datetime.now(UTC).isoformat(),
            "implementation_source": contract["implementation_source"],
            "evidence_package": package,
            "sequence": steps,
            "credential_material_in_evidence": False,
            "identity_material_in_evidence": False,
            "raw_output_retained": False,
            "local_named_credentials_after_run": "left_in_place_for_operator_disposition",
            "passed": passed,
        },
    )
    return 0 if passed else 1


def validate_only(support: Any, contract: dict[str, Any]) -> int:
    repo = support.repo_root()
    target = support.target_root(repo)
    env = dict(os.environ)
    env["CARGO_TARGET_DIR"] = str(target)
    with support.source_checkout(repo, contract["implementation_source"], "live-validation") as source:
        shutil.copy2(
            repo / PROBE_PATH,
            source / "crates/norn/examples/p2_live_refresh_probe.rs",
        )
        norn, _probe = cargo_build(source, target, env, contract["toolchain"])
        shutil.rmtree(norn.parent, ignore_errors=True)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--validate-only", action="store_true")
    args = parser.parse_args()
    try:
        if args.validate_only and args.output is not None:
            raise RuntimeError("--validate-only does not accept --output")
        if not args.validate_only and args.output is None:
            raise RuntimeError("live execution requires --output")
        support, contract = load(require_clean=not args.validate_only)
        if args.validate_only:
            return validate_only(support, contract)
        return execute(args, support, contract)
    except (OSError, RuntimeError, subprocess.CalledProcessError, ValueError) as error:
        print(f"P2 live A/B/A did not complete: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
