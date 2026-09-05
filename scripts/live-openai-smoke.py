#!/usr/bin/env python3
"""Serve one explicitly dispatched live smoke with named prerequisites and no secret logs."""

import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys


MARKER = "NORN_TEST_PREREQUISITE_UNMET"
TARGET = "openai_live_hello_smoke"


def worker_value(name, environment=None, parent_path=None):
    """Use the battery's name-only own/immediate-worker-parent environment seam."""
    environment = os.environ if environment is None else environment
    if name in environment:
        return environment[name]
    path = Path(f"/proc/{os.getppid()}/environ") if parent_path is None else parent_path
    try:
        entries = path.read_bytes().split(b"\0")
    except OSError:
        return None
    prefix = name.encode("ascii") + b"="
    for entry in entries:
        if entry.startswith(prefix):
            try:
                return entry[len(prefix):].decode("utf-8")
            except UnicodeDecodeError:
                return None
    return None


def measure(repo_path, expected_head):
    """Run the exact target behind the source guard; expose only safe evidence."""
    receipt = {
        "expected_head": expected_head, "head_commit": "", "gates_sha256": "",
        "guard_sha256": "", "runner_sha256": "", "verdict": "refused",
        "reason": "", "command_exit": 126, "test_target": TARGET,
    }
    root = Path(repo_path)
    if not root.is_absolute() or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", expected_head):
        receipt["reason"] = "repo_path must be absolute and expected_head a complete Git object ID"
        return receipt
    key = worker_value("OPENAI_TEST_KEY")
    if not key:
        receipt["reason"] = f"{MARKER}: live OpenAI smoke requires nonempty OPENAI_TEST_KEY"
        return receipt
    jobs = worker_value("REPO_BATTERY_LEG_JOBS")
    if jobs is None or not re.fullmatch(r"[1-9][0-9]*", jobs):
        receipt["reason"] = f"{MARKER}: live OpenAI smoke requires positive REPO_BATTERY_LEG_JOBS"
        return receipt
    try:
        head = subprocess.run(["git", "rev-parse", "--verify", "HEAD^{commit}"],
                              cwd=root, capture_output=True, text=True, check=False)
        if head.returncode != 0:
            receipt["reason"] = "git could not resolve HEAD in repo_path"
            return receipt
        receipt["head_commit"] = head.stdout.strip()
        if receipt["head_commit"] != expected_head:
            receipt["reason"] = "repo_path HEAD differs from expected_head"
            return receipt
        for field, relative in (("gates_sha256", "gates.json"),
                                ("guard_sha256", "scripts/source-bound-leg.py"),
                                ("runner_sha256", "scripts/live-openai-smoke.py")):
            receipt[field] = hashlib.sha256((root / relative).read_bytes()).hexdigest()
        environment = dict(os.environ)
        environment["OPENAI_TEST_KEY"] = key
        for name in ("CARGO_BUILD_JOBS", "NEXTEST_TEST_THREADS", "RUST_TEST_THREADS"):
            environment[name] = jobs
        command = [sys.executable, "scripts/source-bound-leg.py", "--", "cargo", "test",
                   "--locked", "-p", "norn", "--features", "live-api-smoke", "--test",
                   "live_openai_smoke", "--", "--exact", TARGET, "--nocapture"]
        result = subprocess.run(command, cwd=root, env=environment, capture_output=True,
                                text=True, errors="replace", check=False)
    except OSError:
        # OS/provider output can contain environment-derived data. The receipt
        # names the failing operation, never dumps raw subprocess diagnostics.
        receipt["reason"] = "live smoke source probe or guarded command could not execute"
        return receipt
    receipt["command_exit"] = result.returncode
    if result.returncode != 0:
        receipt["verdict"] = "red"
        receipt["reason"] = "guarded live_openai_smoke test command returned nonzero"
        return receipt
    records = []
    for line in result.stdout.splitlines():
        if not line.startswith('{"schema": "norn-source-bound-leg.v1"'):
            continue
        try:
            records.append(json.loads(line))
        except json.JSONDecodeError:
            receipt["reason"] = "source guard witness was not valid JSON"
            return receipt
    expected = {"head": expected_head, "dirty": [],
                "declaration_sha256": receipt["gates_sha256"],
                "guard_sha256": receipt["guard_sha256"]}
    valid = (len(records) == 2 and records[0].get("phase") == "before"
             and records[0].get("verdict") == "clean"
             and records[1].get("phase") == "after" and records[1].get("verdict") == "green"
             and records[1].get("command_exit") == 0
             and all(record.get("witness") == expected for record in records))
    if not valid:
        receipt["reason"] = "exact clean before/after source guard witnesses were not established"
        return receipt
    if MARKER in result.stdout or MARKER in result.stderr:
        receipt["reason"] = "live smoke reported an unmet test prerequisite"
        return receipt
    summary = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;"
    if summary not in result.stdout:
        receipt["reason"] = "live smoke did not report exactly one executed passing test"
        return receipt
    receipt["verdict"] = "green"
    receipt["reason"] = "exact-commit live hello smoke observed TextDelta and Done"
    return receipt


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit("usage: live-openai-smoke.py ABSOLUTE_REPO EXPECTED_HEAD")
    print(json.dumps(measure(sys.argv[1], sys.argv[2])), flush=True)
