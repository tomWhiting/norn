"""Provenance and repository-path support for final P3/P4 evidence."""

from __future__ import annotations

import hashlib
import json
import platform
import re
import subprocess
import sys
import time
from datetime import UTC, datetime
from pathlib import Path
from typing import Any


def run(
    args: list[str],
    *,
    cwd: Path,
    env: dict[str, str] | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[bytes]:
    return subprocess.run(
        args,
        cwd=cwd,
        env=env,
        check=check,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )


def git(repo: Path, *args: str) -> bytes:
    return run(["git", *args], cwd=repo).stdout


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def file_sha256(path: Path) -> str:
    return sha256(path.read_bytes())


def commit(repo: Path, value: str) -> str:
    return git(repo, "rev-parse", f"{value}^{{commit}}").decode().strip()


def target_root(repo: Path) -> Path:
    common = Path(
        git(repo, "rev-parse", "--path-format=absolute", "--git-common-dir")
        .decode()
        .strip()
    ).resolve()
    target = common.parent / "target"
    target.mkdir(exist_ok=True)
    if target.is_symlink():
        raise RuntimeError("repository target must not be a symlink")
    return target


def rust_path(path: str) -> bool:
    name = Path(path).name
    return (
        path.endswith(".rs")
        or name
        in {
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "rustfmt.toml",
            "clippy.toml",
        }
        or path == ".cargo/config.toml"
    )


def manifest(repo: Path, source: str) -> list[dict[str, str]]:
    entries: list[dict[str, str]] = []
    for raw in git(repo, "ls-tree", "-r", "-z", source).split(b"\0"):
        if not raw:
            continue
        metadata, path_bytes = raw.split(b"\t", 1)
        mode, kind, blob = metadata.decode().split()
        path = path_bytes.decode()
        if rust_path(path):
            entries.append({"path": path, "mode": mode, "type": kind, "blob": blob})
    return entries


def path_inventory(repo: Path, base: str, source: str) -> dict[str, Any]:
    raw = git(repo, "diff", "--name-only", "-z", f"{base}...{source}")
    paths = [item.decode() for item in raw.split(b"\0") if item]
    return {"count": len(paths), "nul_sha256": sha256(raw), "paths": paths}


def source_support_manifest(
    repo: Path, source: str, fixed_paths: tuple[str, ...]
) -> list[dict[str, str]]:
    paths = list(fixed_paths)
    policy_paths = (
        git(
            repo,
            "ls-tree",
            "-r",
            "--name-only",
            source,
            "--",
            "docs/reviews/evidence",
        )
        .decode()
        .splitlines()
    )
    paths.extend(
        path
        for path in policy_paths
        if (Path(path).name.startswith("p0_") and path.endswith(".py"))
        or (Path(path).name.startswith("p0-") and path.endswith(".yml"))
    )
    result = []
    for path in sorted(set(paths)):
        data = git(repo, "show", f"{source}:{path}")
        if sha256(data) != file_sha256(repo / path):
            raise RuntimeError(f"working evidence support differs from source: {path}")
        result.append({"path": path, "sha256": sha256(data)})
    return result


def verify_reused_artifacts(
    repo: Path, source: str, expected_artifacts: dict[str, str]
) -> list[dict[str, str]]:
    result = []
    for path, expected in expected_artifacts.items():
        source_bytes = git(repo, "show", f"{source}:{path}")
        if sha256(source_bytes) != expected or file_sha256(repo / path) != expected:
            raise RuntimeError(f"accepted artifact hash mismatch: {path}")
        result.append({"path": path, "sha256": expected})
    return result


def command_text(repo: Path, args: list[str]) -> str:
    recorded = list(args)
    if (
        recorded
        and Path(recorded[0]).is_absolute()
        and Path(recorded[0]).name == "cargo"
    ):
        recorded[0] = "<pinned-cargo>"
    return (
        " ".join(recorded)
        .replace(str(target_root(repo)), "<repo>/target")
        .replace(str(repo), "<repo>")
    )


def execute_case(
    repo: Path,
    scratch: Path,
    env: dict[str, str],
    name: str,
    args: list[str],
    require_tests: bool,
    expected_tests: int | None = None,
) -> dict[str, Any]:
    started = datetime.now(UTC).isoformat()
    before = time.monotonic()
    completed = run(args, cwd=repo, env=env, check=False)
    elapsed = round(time.monotonic() - before, 3)
    matches = re.findall(
        rb"test result: (?:ok|FAILED)\. (\d+) passed;", completed.stdout
    )
    observed = sum(int(value) for value in matches) if matches else None
    passed = (
        completed.returncode == 0
        and (not require_tests or bool(observed))
        and (expected_tests is None or observed == expected_tests)
    )
    (scratch / f"{name}.log").write_bytes(completed.stdout)
    if not passed:
        tail = completed.stdout.decode(errors="replace").splitlines()[-120:]
        print(f"{name} failed\n" + "\n".join(tail), file=sys.stderr)
    return {
        "id": name,
        "command": command_text(repo, args),
        "started_at": started,
        "elapsed_seconds": elapsed,
        "exit_status": completed.returncode,
        "observed_tests": observed,
        "output_sha256": sha256(completed.stdout),
        "result": "pass" if passed else "fail",
    }


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def display_path(repo: Path, path: Path) -> str:
    resolved = path.resolve()
    try:
        return str(resolved.relative_to(repo.resolve()))
    except ValueError:
        try:
            suffix = resolved.relative_to(target_root(repo))
        except ValueError as error:
            raise RuntimeError(
                "evidence path is outside the repository target"
            ) from error
        return str(Path("<repo>/target") / suffix)


def validate_output_paths(args: Any, repo: Path) -> None:
    allowed = target_root(repo).resolve()
    resolved = []
    for name in ("output", "policy_output", "gate", "policy", "distributions"):
        path = getattr(args, name, None)
        if path is not None:
            path = Path(path).resolve()
            setattr(args, name, path)
            resolved.append(path)
            if path != allowed and allowed not in path.parents:
                raise RuntimeError(
                    f"{name} must stay under the primary repository target"
                )
    if len(resolved) != len(set(resolved)):
        raise RuntimeError("final evidence artifact paths must be distinct")


def clear_output_paths(args: Any) -> None:
    for name in ("output", "gate", "policy", "distributions"):
        path = getattr(args, name)
        if path.is_symlink() or path.is_file():
            path.unlink()
        elif path.exists():
            raise RuntimeError(f"{name} output is not a removable file")


def environment_record(
    repo: Path,
    env: dict[str, str],
    removed: int,
    toolchain: str,
    policy_support: Any,
) -> dict[str, Any]:
    toolchain_bin = Path(env["RUSTC"]).parent
    pinned_names = (
        "cargo",
        "cargo-clippy",
        "cargo-fmt",
        "clippy-driver",
        "rustc",
        "rustdoc",
        "rustfmt",
    )
    return {
        "cargo": run([str(toolchain_bin / "cargo"), "--version"], cwd=repo, env=env)
        .stdout.decode()
        .strip(),
        "rustc": run([env["RUSTC"], "--version", "--verbose"], cwd=repo, env=env)
        .stdout.decode()
        .strip(),
        "platform": platform.platform(),
        "python": platform.python_version(),
        "target": "<repo>/target",
        "controls": policy_support.PATH_FREE_ENVIRONMENT_CONTROLS,
        "sanitized_variable_names": sorted(env),
        "removed_ambient_variable_count": removed,
        "fingerprint": policy_support.environment_fingerprint(env, toolchain),
        "pinned_toolchain": {
            name: {"sha256": file_sha256(toolchain_bin / name)} for name in pinned_names
        },
        "cargo_config": policy_support.cargo_config_fingerprints(repo, env),
        "loopback_fixtures": "native-host execution required",
    }


def gate_records_valid(records: object, expected: list[tuple[str, str, bool]]) -> bool:
    if not isinstance(records, list) or len(records) != len(expected):
        return False
    for record, (case_id, command, require_tests) in zip(
        records, expected, strict=True
    ):
        if not isinstance(record, dict):
            return False
        observed = record.get("observed_tests")
        if (
            record.get("id") != case_id
            or record.get("command") != command
            or record.get("exit_status") != 0
            or record.get("result") != "pass"
            or not isinstance(record.get("output_sha256"), str)
            or len(record["output_sha256"]) != 64
            or (require_tests and (not isinstance(observed, int) or observed < 1))
        ):
            return False
    return True


def distribution_records_valid(
    document: dict[str, Any], tests: tuple[str, ...], commands: list[str]
) -> bool:
    cases = document.get("cases")
    if not isinstance(cases, list) or len(cases) != len(tests):
        return False
    for case, test, command in zip(cases, tests, commands, strict=True):
        if (
            not isinstance(case, dict)
            or case.get("test") != test
            or case.get("runs") != 20
        ):
            return False
        observations = case.get("observations")
        if not isinstance(observations, list) or len(observations) != 20:
            return False
        for index, record in enumerate(observations, 1):
            if (
                not isinstance(record, dict)
                or record.get("iteration") != index
                or record.get("id") != f"{test.rsplit('::', 1)[-1]}-{index}"
                or record.get("command") != command
                or record.get("exit_status") != 0
                or record.get("observed_tests") != 1
                or record.get("result") != "pass"
                or not isinstance(record.get("output_sha256"), str)
                or len(record["output_sha256"]) != 64
            ):
                return False
        if case.get("passed") is not True:
            return False
    return document.get("observations") == len(tests) * 20
