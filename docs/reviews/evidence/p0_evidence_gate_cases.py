"""Pinned Gate C case construction for the integrated evidence runner."""

from __future__ import annotations

import sys
from collections.abc import Callable
from pathlib import Path

import p0_evidence_manifest as manifest
from p0_evidence_support import Case


CargoCommand = Callable[..., tuple[str, ...]]


def require_self_test_modules(actual_modules: tuple[str, ...]) -> None:
    if actual_modules != manifest.EVIDENCE_SELF_TEST_MODULES:
        raise RuntimeError("P0 evidence self-test module inventory changed")


def gate_cases(
    policy_output: Path,
    head: str,
    cargo: CargoCommand,
    base: str,
    python_executable: str | None = None,
) -> list[Case]:
    """Build the exact compiler, test, policy, and disclosure gate inventory."""
    actual_modules = tuple(
        sorted(path.name for path in Path(__file__).parent.glob("test_p0_*.py"))
    )
    require_self_test_modules(actual_modules)
    python = python_executable if python_executable is not None else sys.executable
    cases = [
        Case("fmt", "compiler", cargo("fmt", "--all", "--", "--check")),
        Case(
            "strict_clippy",
            "compiler",
            cargo("clippy", "--workspace", "--all-targets", "--", "-D", "warnings"),
        ),
        Case(
            "workspace_check",
            "compiler",
            cargo("check", "--workspace", "--all-targets"),
        ),
        Case(
            "workspace_all_targets",
            "tests",
            cargo("test", "--workspace", "--all-targets", "--quiet"),
            minimum_tests=1,
        ),
        Case(
            "norn_tests",
            "tests",
            cargo("test", "-p", "norn", "--tests", "--quiet"),
            minimum_tests=1,
        ),
        Case(
            "norn_cli_tests",
            "tests",
            cargo("test", "-p", "norn-cli", "--tests", "--quiet"),
            minimum_tests=1,
        ),
        Case(
            "norn_tui_tests",
            "tests",
            cargo("test", "-p", "norn-tui", "--tests", "--quiet"),
            minimum_tests=1,
        ),
        Case(
            "workspace_docs",
            "docs",
            cargo("test", "--workspace", "--doc", "--quiet"),
            minimum_tests=1,
        ),
        Case(
            "norn_test_utils_docs",
            "docs",
            cargo("test", "-p", "norn", "--doc", "--features", "test-utils"),
            expected_tests=8,
            expected_test_names=manifest.DOCTEST_NAMES,
        ),
        Case(
            "phase_diff_check", "policy", ("git", "diff", "--check", f"{base}...{head}")
        ),
        Case(
            "evidence_tooling_self_tests",
            "policy",
            (
                python,
                "-B",
                "-I",
                "-m",
                "unittest",
                "discover",
                "-s",
                "docs/reviews/evidence",
                "-p",
                "test_p0_*.py",
            ),
            recorded_command=(
                "<python>",
                "-B",
                "-I",
                "-m",
                "unittest",
                "discover",
                "-s",
                "docs/reviews/evidence",
                "-p",
                "test_p0_*.py",
            ),
            expected_tool_tests=manifest.EVIDENCE_SELF_TEST_COUNT,
            expected_tool_test_modules=manifest.EVIDENCE_SELF_TEST_MODULES,
        ),
        Case(
            "full_range_policy",
            "policy",
            (
                python,
                "-B",
                "-I",
                "docs/reviews/evidence/run_p0_policy_evidence.py",
                "--base",
                base,
                "--head",
                head,
                "--output",
                str(policy_output.resolve()),
            ),
            recorded_command=(
                "<python>",
                "-B",
                "-I",
                "docs/reviews/evidence/run_p0_policy_evidence.py",
                "--base",
                base,
                "--head",
                head,
                "--output",
                "<policy-output>",
            ),
        ),
    ]
    for name in manifest.SECRET_SENTINELS:
        cases.append(
            Case(
                name,
                "non_disclosure",
                cargo("test", "-p", "norn", "--lib", name, "--", "--exact"),
                expected_tests=1,
                expected_test_names=(name,),
            )
        )
    for name in manifest.MODEL_FACING_SENTINELS:
        cases.append(
            Case(
                name,
                "model_facing_non_disclosure",
                cargo("test", "-p", "norn", "--lib", name, "--", "--exact"),
                expected_tests=1,
                expected_test_names=(name,),
            )
        )
    return cases
