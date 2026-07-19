"""Historical policy-scan binding for the P2 evidence package."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Final


POLICY_RUNNER: Final = "docs/reviews/evidence/run_p0_policy_evidence.py"
POLICY_SUPPORT: Final = (
    POLICY_RUNNER,
    "docs/reviews/evidence/p0-rust-items.yml",
    "docs/reviews/evidence/p0_artifact_writers.py",
    "docs/reviews/evidence/p0_evidence_paths.py",
    "docs/reviews/evidence/p0_module_references.py",
    "docs/reviews/evidence/p0_module_shape.py",
    "docs/reviews/evidence/p0_policy_rust.py",
    "docs/reviews/evidence/p0_rust_literals.py",
    "docs/reviews/evidence/fixtures/mod_shape/allowed.rs",
    "docs/reviews/evidence/fixtures/mod_shape/rejected.rs",
    "docs/reviews/evidence/fixtures/module_references/included/mod.rs",
    "docs/reviews/evidence/fixtures/module_references/owner.rs",
    "docs/reviews/evidence/fixtures/module_references/production_only/mod.rs",
    "docs/reviews/evidence/fixtures/module_references/shared/mod.rs",
    "docs/reviews/evidence/fixtures/module_references/test_included/mod.rs",
    "docs/reviews/evidence/fixtures/module_references/test_only/mod.rs",
)


def command(
    python: str,
    repo: Path,
    policy_source: Path,
    output: Path,
    contract: dict[str, Any],
) -> list[str]:
    return [
        python,
        "-I",
        "-S",
        "-B",
        str(repo / POLICY_RUNNER),
        "--base",
        contract["base"],
        "--head",
        contract["implementation_source"],
        "--output",
        str(output.resolve()),
        "--repository",
        str(policy_source),
    ]
