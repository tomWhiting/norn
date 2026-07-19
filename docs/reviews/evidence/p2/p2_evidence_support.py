"""Source, command, and artifact support for retained P2 evidence."""

from __future__ import annotations

import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import tarfile
import time
from contextlib import contextmanager
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Iterator


AUTH_PREFIXES = (
    "crates/norn/src/provider/openai_oauth/",
    "crates/norn/src/provider/auth/",
)
AUTH_PATHS = frozenset(
    {
        "crates/norn/src/provider/auth.rs",
        "crates/norn/src/config/provider_auth.rs",
        "crates/norn/src/config/provider_auth_tests.rs",
        "crates/norn/tests/provider_auth_policy_api.rs",
        "crates/norn-cli/src/commands/auth.rs",
        "crates/norn-cli/src/commands/auth_foreign_home_tests.rs",
        "crates/norn-cli/src/commands/auth_state_matrix_tests.rs",
        "crates/norn-cli/src/commands/doctor.rs",
        "crates/norn-cli/src/commands/doctor_tests.rs",
        "crates/norn-cli/src/config/provider_auth.rs",
        "crates/norn-cli/src/config/provider_auth_tests.rs",
        "crates/norn-cli/src/print/provider.rs",
        "crates/norn-cli/src/print/provider_tests.rs",
    }
)


def repo_root() -> Path:
    return Path(__file__).resolve().parents[4]


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


def require_clean_package(repo: Path) -> None:
    """Reject tracked or nonignored changes; ignored files cannot enter the archive."""
    if git(repo, "status", "--porcelain", "--untracked-files=all"):
        raise RuntimeError("P2 evidence requires a clean package worktree")


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


def load_contract(path: Path) -> dict[str, Any]:
    def reject_duplicate(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise ValueError("contract contains a duplicate key")
            result[key] = value
        return result

    value = json.loads(path.read_bytes(), object_pairs_hook=reject_duplicate)
    required = {
        "schema_version",
        "phase",
        "base",
        "implementation_source",
        "integration_anchor",
        "toolchain",
        "distribution_runs",
        "phase_cases",
        "distribution_tests",
        "historical_artifacts",
    }
    if not isinstance(value, dict) or set(value) != required:
        raise RuntimeError("P2 evidence contract shape changed")
    if value["schema_version"] != 1 or value["phase"] != "P2":
        raise RuntimeError("P2 evidence contract identity changed")
    if value["distribution_runs"] != 20:
        raise RuntimeError("P2 distribution denominator changed")
    if len(value["phase_cases"]) != 15 or len(value["distribution_tests"]) != 9:
        raise RuntimeError("P2 evidence inventory changed")
    return value


def path_inventory(repo: Path, base: str, source: str) -> dict[str, Any]:
    raw = git(repo, "diff", "--name-only", "-z", f"{base}...{source}")
    paths = [item.decode() for item in raw.split(b"\0") if item]
    return {"count": len(paths), "nul_sha256": sha256(raw), "paths": paths}


def rust_manifest(repo: Path, source: str) -> list[dict[str, str]]:
    result = []
    for raw in git(repo, "ls-tree", "-r", "-z", source).split(b"\0"):
        if not raw:
            continue
        metadata, encoded_path = raw.split(b"\t", 1)
        mode, kind, blob = metadata.decode().split()
        path = encoded_path.decode()
        if path.endswith(".rs") or Path(path).name in {
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "rustfmt.toml",
            "clippy.toml",
        }:
            result.append({"path": path, "mode": mode, "type": kind, "blob": blob})
    return result


def auth_path_inventory(
    repo: Path, base: str, implementation: str, integration: str, package: str
) -> dict[str, Any]:
    paths = path_inventory(repo, base, implementation)["paths"]
    owned = sorted(
        path
        for path in paths
        if path in AUTH_PATHS or path.startswith(AUTH_PREFIXES)
    )
    records = []
    for path in owned:
        blobs = [
            git(repo, "rev-parse", f"{source}:{path}").decode().strip()
            for source in (implementation, integration, package)
        ]
        if len(set(blobs)) != 1:
            raise RuntimeError(f"P2 auth product path changed after candidate: {path}")
        records.append({"path": path, "blob": blobs[0]})
    raw = "".join(f"{item['blob']} {item['path']}\0" for item in records).encode()
    return {"count": len(records), "nul_sha256": sha256(raw), "paths": records}


def command_text(repo: Path, args: list[str]) -> str:
    rendered = list(args)
    if rendered and Path(rendered[0]).is_absolute():
        if Path(rendered[0]).name in {"cargo", "rustc", "rustdoc"}:
            rendered[0] = f"<pinned-{Path(rendered[0]).name}>"
        elif rendered[0] == os.fsdecode(os.fsencode(os.sys.executable)):
            rendered[0] = "<python>"
    return (
        " ".join(rendered)
        .replace(str(target_root(repo)), "<repo>/target")
        .replace(str(repo), "<repo>")
    )


def execute_case(
    repo: Path,
    cwd: Path,
    scratch: Path,
    env: dict[str, str],
    case_id: str,
    args: list[str],
    require_tests: bool,
    expected_tests: int | None,
) -> dict[str, Any]:
    started = datetime.now(UTC).isoformat()
    before = time.monotonic()
    completed = run(args, cwd=cwd, env=env, check=False)
    elapsed = round(time.monotonic() - before, 3)
    matches = re.findall(rb"test result: (?:ok|FAILED)\. (\d+) passed;", completed.stdout)
    python_matches = re.findall(rb"Ran (\d+) tests?", completed.stdout)
    observed_matches = matches or python_matches
    observed = sum(int(value) for value in observed_matches) if observed_matches else None
    passed = (
        completed.returncode == 0
        and (not require_tests or isinstance(observed, int) and observed > 0)
        and (expected_tests is None or observed == expected_tests)
    )
    (scratch / f"{case_id}.log").write_bytes(completed.stdout)
    if not passed:
        tail = completed.stdout.decode(errors="replace").splitlines()[-80:]
        print(f"{case_id} failed\n" + "\n".join(tail), file=os.sys.stderr)
    return {
        "id": case_id,
        "command": command_text(repo, args),
        "started_at": started,
        "elapsed_seconds": elapsed,
        "exit_status": completed.returncode,
        "observed_tests": observed,
        "required_tests": require_tests,
        "expected_tests": expected_tests,
        "output_sha256": sha256(completed.stdout),
        "result": "pass" if passed else "fail",
    }


def write_json(path: Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    try:
        with temporary.open("x", encoding="utf-8") as handle:
            json.dump(value, handle, indent=2, sort_keys=True)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        if temporary.exists():
            temporary.unlink()


@contextmanager
def source_checkout(repo: Path, source: str, label: str) -> Iterator[Path]:
    root = target_root(repo) / "build" / f"p2-{label}-{source[:8]}-{os.getpid()}"
    root.mkdir(parents=True, exist_ok=False)
    try:
        archive = git(repo, "archive", "--format=tar", source)
        with tarfile.open(fileobj=io.BytesIO(archive), mode="r:") as bundle:
            members = bundle.getmembers()
            if any(not (member.isfile() or member.isdir()) for member in members):
                raise RuntimeError("source archive contains a non-file entry")
            bundle.extractall(root, members=members, filter="data")
        yield root
    finally:
        shutil.rmtree(root, ignore_errors=True)
