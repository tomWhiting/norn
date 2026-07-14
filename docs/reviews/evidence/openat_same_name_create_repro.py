#!/usr/bin/env python3
"""Reproduce concurrent same-name openat(O_CREAT) behavior.

Each worker independently opens the parent directory before a barrier, matching
Norn's PrivateRoot topology. The JSON result reports the complete denominator,
errno distribution, retry count, and whether the target existed when a worker
observed ENOENT. It does not infer which racing syscall created the target.

Examples:

  python3 openat_same_name_create_repro.py --trials 100
  python3 openat_same_name_create_repro.py --trials 100 --enoent-retries 1
  python3 openat_same_name_create_repro.py --trials 100 --absolute
  python3 openat_same_name_create_repro.py --trials 100 --different-names
  python3 openat_same_name_create_repro.py --trials 100 --exclusive
"""

from __future__ import annotations

import argparse
import collections
import errno
import json
import os
import platform
import tempfile
import threading
from pathlib import Path

from p0_evidence_paths import RepositoryTargetLayout


def positive_integer(value: str) -> int:
    parsed = int(value)
    if parsed < 1:
        raise argparse.ArgumentTypeError("value must be at least 1")
    return parsed


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--trials", required=True, type=positive_integer)
    parser.add_argument(
        "--threads",
        type=positive_integer,
        default=4,
        help="workers per trial; 4 matches the Norn convergence regression",
    )
    parser.add_argument(
        "--enoent-retries",
        type=int,
        choices=range(0, 3),
        default=0,
        metavar="{0,1,2}",
    )
    parser.add_argument("--absolute", action="store_true")
    parser.add_argument("--different-names", action="store_true")
    parser.add_argument("--exclusive", action="store_true")
    parser.add_argument("--precreate", action="store_true")
    return parser.parse_args()


def directory_flags() -> int:
    return (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )


def file_flags(exclusive: bool) -> int:
    flags = (
        os.O_WRONLY
        | os.O_CREAT
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_NOFOLLOW", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    if exclusive:
        flags |= os.O_EXCL
    return flags


def run_trial(
    root: Path,
    trial: int,
    args: argparse.Namespace,
) -> list[dict[str, object]]:
    trial_dir = root / f"trial-{trial}"
    trial_dir.mkdir(mode=0o700)
    shared_name = "index.lock"
    if args.precreate:
        (trial_dir / shared_name).touch(mode=0o600)

    barrier = threading.Barrier(args.threads)
    results: list[dict[str, object] | None] = [None] * args.threads

    def worker(worker_id: int) -> None:
        name = f"index-{worker_id}.lock" if args.different_names else shared_name
        path = trial_dir / name
        parent_fd = os.open(trial_dir, directory_flags())
        retries = 0
        try:
            barrier.wait()
            while True:
                try:
                    if args.absolute:
                        opened_fd = os.open(path, file_flags(args.exclusive), 0o600)
                    else:
                        opened_fd = os.open(
                            name,
                            file_flags(args.exclusive),
                            0o600,
                            dir_fd=parent_fd,
                        )
                    os.close(opened_fd)
                    results[worker_id] = {
                        "outcome": "success",
                        "retries": retries,
                    }
                    return
                except OSError as error:
                    if error.errno == errno.ENOENT and retries < args.enoent_retries:
                        retries += 1
                        continue
                    results[worker_id] = {
                        "outcome": "errno",
                        "errno": error.errno,
                        "errno_name": errno.errorcode.get(error.errno, "UNKNOWN"),
                        "retries": retries,
                        "target_exists": path.exists(),
                    }
                    return
        # Preserve only path-free structure for unexpected worker failures.
        except BaseException as error:
            results[worker_id] = {
                "outcome": "worker_error",
                "error_type": type(error).__name__,
                "errno": error.errno if isinstance(error, OSError) else None,
                "retries": retries,
            }
        finally:
            os.close(parent_fd)

    workers = [
        threading.Thread(target=worker, args=(worker_id,))
        for worker_id in range(args.threads)
    ]
    for worker_thread in workers:
        worker_thread.start()
    for worker_thread in workers:
        worker_thread.join()

    return [
        result
        if result is not None
        else {"outcome": "worker_error", "error": "worker produced no result"}
        for result in results
    ]


def main() -> int:
    args = parse_args()
    all_results: list[dict[str, object]] = []
    repo = Path(__file__).resolve().parents[3]
    scratch_parent = RepositoryTargetLayout.locate(repo).target_root / "evidence"
    scratch_parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix="norn-openat-repro-", dir=scratch_parent
    ) as temp:
        root = Path(temp)
        for trial in range(args.trials):
            all_results.extend(run_trial(root, trial, args))

    outcomes = collections.Counter(str(result["outcome"]) for result in all_results)
    errnos = collections.Counter(
        str(result["errno_name"])
        for result in all_results
        if result["outcome"] == "errno"
    )
    summary = {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "trials": args.trials,
        "threads_per_trial": args.threads,
        "thread_attempts": args.trials * args.threads,
        "absolute": args.absolute,
        "different_names": args.different_names,
        "exclusive": args.exclusive,
        "precreate": args.precreate,
        "enoent_retry_limit": args.enoent_retries,
        "outcomes": dict(sorted(outcomes.items())),
        "errno_counts": dict(sorted(errnos.items())),
        "retries_used": sum(int(result.get("retries", 0)) for result in all_results),
        "enoent_with_target_observed": sum(
            1
            for result in all_results
            if result.get("errno_name") == "ENOENT"
            and result.get("target_exists") is True
        ),
        "worker_errors": [
            result for result in all_results if result["outcome"] == "worker_error"
        ],
    }
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 1 if summary["worker_errors"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
