"""Argument parsing for the integrated P0 evidence runner."""

from __future__ import annotations

import argparse
from pathlib import Path


def parse_runner_args(
    minimum_repeated_runs: int, minimum_concurrency_runs: int
) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run provenance-bearing P0 gate and distribution evidence."
    )
    parser.add_argument(
        "--target-dir",
        required=True,
        type=Path,
        help="fresh Cargo target directory under the main repository's target/build/",
    )
    parser.add_argument("--output", required=True, type=Path)
    subparsers = parser.add_subparsers(dest="mode", required=True)
    gate = subparsers.add_parser(
        "gate", help="run compiler, test, non-disclosure, and policy evidence"
    )
    gate.add_argument("--policy-output", required=True, type=Path)
    distributions = subparsers.add_parser(
        "distributions",
        help="run the prescribed concurrency and sensitive-seam distributions",
    )
    distributions.add_argument(
        "--concurrency-runs",
        type=_minimum_value("concurrency runs", minimum_concurrency_runs),
        default=minimum_concurrency_runs,
    )
    distributions.add_argument(
        "--other-runs",
        type=_minimum_value("repeated runs", minimum_repeated_runs),
        default=minimum_repeated_runs,
    )
    return parser.parse_args()


def _minimum_value(label: str, minimum: int):
    def parse(value: str) -> int:
        parsed = int(value)
        if parsed < minimum:
            raise argparse.ArgumentTypeError(f"{label} must be at least {minimum}")
        return parsed

    return parse
