#!/usr/bin/env python3
"""Run source-bound distribution evidence for the AFFINITY-01 candidate."""

from __future__ import annotations

import argparse
from datetime import UTC, datetime
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
from typing import Final


BASE: Final = "5e04281542b546c8d831907831b7cdab92edb4f4"
BRANCH: Final = "codex/p5-credential-affinity"
TOOLCHAIN: Final = "1.94.0"
RUNS: Final = 20
EVIDENCE_DIRECTORY: Final = Path("docs/reviews/evidence/p5-affinity")
REPEATED_TESTS: Final = (
    "session::provider_affinity_tests::sinkless_concurrent_first_bind_has_one_immutable_winner",
    "session::provider_affinity_tests::concurrent_legacy_adoption_converges_on_one_identity",
)
SINGLE_TESTS: Final = (
    "session::provider_affinity_tests::interrupted_adoption_leaves_boundary_before_any_identity_binding",
    "r#loop::helpers::tests::context_edited_prompt_respects_provider_epoch_boundaries",
    "r#loop::runner::turn_context_tests::first_sinkless_identity_adoption_cuts_the_existing_response_anchor",
    "session::provider_affinity_embedder_tests::failed_sink_adoption_leaves_identity_and_history_unchanged",
    "session::provider_affinity_embedder_tests::ambiguous_sink_adoption_retries_the_exact_boundary",
    "session::provider_affinity_embedder_tests::stale_managed_sink_cannot_append_after_another_handle_adopts",
    "session::provider_affinity_embedder_tests::persistent_child_inherits_identity_and_stale_parent_cannot_publish",
    "session::provider_affinity_embedder_tests::affinity_fork_of_empty_source_rejects_without_binding_or_publication",
)
CLI_UNIT_TEST: Final = (
    "print::provider::tests::missing_oauth_credentials_keep_login_guidance_and_auth_exit"
)
CLI_TEST: Final = "list_and_export_json_omit_durable_provider_identity"
TEST_RESULT = re.compile(
    rb"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; "
    rb"(\d+) ignored; (\d+) measured; (\d+) filtered out"
)
FULL_COMMIT = re.compile(r"[0-9a-f]{40}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", required=True, help="exact committed source HEAD")
    parser.add_argument("--source-tree", required=True, help="exact source tree object")
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


def nul_paths(raw: bytes) -> list[str]:
    if not raw:
        return []
    if not raw.endswith(b"\0"):
        raise RuntimeError("NUL-delimited path inventory is truncated")
    return [entry.decode() for entry in raw[:-1].split(b"\0")]


def validate_output(repo: Path, output: Path) -> Path:
    resolved = (repo / output).resolve() if not output.is_absolute() else output.resolve()
    allowed = (repo / EVIDENCE_DIRECTORY).resolve()
    if resolved.parent != allowed or resolved.suffix != ".json":
        raise RuntimeError(f"output must be a JSON file directly under {EVIDENCE_DIRECTORY}")
    return resolved


def verify_source(repo: Path, source: str, source_tree: str) -> dict[str, object]:
    if not FULL_COMMIT.fullmatch(source) or not FULL_COMMIT.fullmatch(source_tree):
        raise RuntimeError("source and source-tree must be full 40-character object IDs")
    branch = git(repo, "branch", "--show-current").decode().strip()
    if branch != BRANCH:
        raise RuntimeError(f"AFFINITY-01 evidence must run on {BRANCH}, not {branch!r}")
    if exact_commit(repo, "HEAD") != source:
        raise RuntimeError("HEAD does not match the requested AFFINITY-01 source commit")
    if exact_commit(repo, BASE) != BASE or source == BASE:
        raise RuntimeError("AFFINITY-01 base/source boundary is invalid")
    if git(repo, "merge-base", BASE, source).decode().strip() != BASE:
        raise RuntimeError("AFFINITY-01 source does not descend directly from the exact base")
    observed_tree = git(repo, "rev-parse", "HEAD^{tree}").decode().strip()
    if observed_tree != source_tree:
        raise RuntimeError(f"source tree mismatch: expected {source_tree}, got {observed_tree}")

    dirty_rust = nul_paths(git(repo, "diff", "--name-only", "-z", "HEAD", "--", "*.rs"))
    untracked_rust = nul_paths(
        git(repo, "ls-files", "--others", "--exclude-standard", "-z", "--", "*.rs")
    )
    if dirty_rust or untracked_rust:
        paths = sorted(set(dirty_rust + untracked_rust))
        raise RuntimeError("dirty Rust invalidates AFFINITY-01 evidence:\n" + "\n".join(paths))

    return {
        "base": BASE,
        "branch": branch,
        "source": source,
        "source_tree": source_tree,
        "dirty_rust_paths": [],
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
    ):
        environment.pop(name, None)
    environment["CARGO_TARGET_DIR"] = str(target)
    environment["PYTHONDONTWRITEBYTECODE"] = "1"
    return environment


def cargo(*arguments: str) -> list[str]:
    return ["cargo", f"+{TOOLCHAIN}", "--locked", *arguments]


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
    case_id: str,
    command: list[str],
    runs: int,
) -> dict[str, object]:
    observations = []
    for iteration in range(1, runs + 1):
        print(f"{case_id}: {iteration}/{runs}", flush=True)
        observations.append(execute(repo, environment, command, iteration))
    passed = sum(item["result"] == "pass" for item in observations)
    return {
        "id": case_id,
        "command": " ".join(command),
        "runs": runs,
        "passed": passed,
        "failed": runs - passed,
        "observations": observations,
    }


def library_command(test: str) -> list[str]:
    return cargo("test", "-p", "norn", "--lib", test, "--", "--exact")


def main() -> int:
    args = parse_args()
    repo = repo_root()
    output = validate_output(repo, args.output)
    provenance = verify_source(repo, args.source, args.source_tree)
    environment = evidence_environment(repo)

    distributions = [
        run_case(
            repo,
            environment,
            case_id=test.rsplit("::", 1)[-1],
            command=library_command(test),
            runs=RUNS,
        )
        for test in REPEATED_TESTS
    ]
    deterministic = [
        run_case(
            repo,
            environment,
            case_id=test.rsplit("::", 1)[-1],
            command=library_command(test),
            runs=1,
        )
        for test in SINGLE_TESTS
    ]
    cli_unit = run_case(
        repo,
        environment,
        case_id=CLI_UNIT_TEST.rsplit("::", 1)[-1],
        command=cargo(
            "test",
            "-p",
            "norn-cli",
            "--lib",
            CLI_UNIT_TEST,
            "--",
            "--exact",
        ),
        runs=1,
    )
    observer = run_case(
        repo,
        environment,
        case_id=CLI_TEST,
        command=cargo(
            "test",
            "-p",
            "norn-cli",
            "--test",
            "session_output_redaction",
            "--",
            "--exact",
            CLI_TEST,
        ),
        runs=1,
    )
    all_cases = [*distributions, *deterministic, cli_unit, observer]
    passed = sum(case["passed"] for case in all_cases)
    total = sum(case["runs"] for case in all_cases)
    result = {
        "schema_version": 1,
        "kind": "p5_affinity_01_distributions",
        "generated_at": datetime.now(UTC).isoformat(),
        "provenance": provenance,
        "runner": {
            "path": str(Path(__file__).resolve().relative_to(repo)),
            "sha256": file_sha256(Path(__file__)),
        },
        "environment": {
            "cargo_target_directory": "<repo>/target",
            "rustc": run(
                ["rustc", f"+{TOOLCHAIN}", "--version"],
                cwd=repo,
                environment=environment,
                check=True,
            ).stdout.decode().strip(),
        },
        "totals": {"runs": total, "passed": passed, "failed": total - passed},
        "distribution_cases": distributions,
        "deterministic_cases": deterministic,
        "cli_unit_case": cli_unit,
        "observer_case": observer,
        "result": "pass" if passed == total else "fail",
        "boundary": "AFFINITY-01 candidate evidence only; independent review is required",
    }
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return 0 if passed == total else 1


if __name__ == "__main__":
    raise SystemExit(main())
