"""Execution and provenance support for retained P0 evidence."""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class Case:
    case_id: str
    group: str
    command: tuple[str, ...]
    runs: int = 1
    expected_tests: int | None = None
    minimum_tests: int = 0
    expected_test_names: tuple[str, ...] = ()


def strict_json_loads(raw: bytes) -> object:
    def reject_constant(value: str) -> object:
        raise ValueError(f"non-finite JSON number is forbidden: {value}")

    def reject_duplicate_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON key is forbidden: {key}")
            result[key] = value
        return result

    return json.loads(
        raw,
        parse_constant=reject_constant,
        object_pairs_hook=reject_duplicate_keys,
    )


def checked_output(
    root: Path,
    *command: str,
    environment: dict[str, str] | None = None,
) -> str:
    result = subprocess.run(
        command,
        cwd=root,
        check=True,
        capture_output=True,
        text=True,
        env=environment,
    )
    return result.stdout.strip()


def require_external_path(root: Path, path: Path, label: str) -> None:
    resolved_root = root.resolve()
    resolved_path = path.resolve()
    if resolved_path == resolved_root or resolved_root in resolved_path.parents:
        raise RuntimeError(f"{label} must be outside the evidence worktree")


def prepare_fresh_target_dir(target_dir: Path) -> None:
    resolved = target_dir.resolve()
    if resolved.exists():
        if not resolved.is_dir():
            raise RuntimeError("Cargo target path must be a directory")
        if next(resolved.iterdir(), None) is not None:
            raise RuntimeError("P0 evidence requires an empty Cargo target directory")
        return
    resolved.mkdir(parents=True, mode=0o700)


def evidence_environment(target_dir: Path) -> tuple[dict[str, str], list[str]]:
    source = os.environ
    resolved_target = target_dir.resolve()
    sterile_home = resolved_target / "evidence-home"
    temporary_root = resolved_target / "evidence-tmp"
    sterile_home.mkdir(parents=True, exist_ok=True, mode=0o700)
    temporary_root.mkdir(parents=True, exist_ok=True, mode=0o700)
    source_home = Path(source.get("HOME", str(Path.home()))).expanduser()
    cargo_home = Path(
        source.get("CARGO_HOME", str(source_home / ".cargo"))
    ).expanduser()
    rustup_home = Path(
        source.get("RUSTUP_HOME", str(source_home / ".rustup"))
    ).expanduser()
    environment = {
        "CARGO_HOME": str(cargo_home.resolve()),
        "CARGO_INCREMENTAL": "0",
        "CARGO_NET_OFFLINE": "true",
        "CARGO_TARGET_DIR": str(resolved_target),
        "CARGO_TERM_COLOR": "never",
        "GIT_CONFIG_NOSYSTEM": "1",
        "HOME": str(sterile_home),
        "LANG": "C",
        "LC_ALL": "C",
        "NO_COLOR": "1",
        "PATH": source.get("PATH", os.defpath),
        "RUSTUP_HOME": str(rustup_home.resolve()),
        "TERM": "dumb",
        "TMPDIR": str(temporary_root),
        "TZ": "UTC",
    }
    return environment, sorted(set(source) - set(environment))


def repository_state(
    root: Path,
    environment: dict[str, str] | None = None,
) -> dict[str, str]:
    return {
        "head": checked_output(
            root, "git", "rev-parse", "HEAD", environment=environment
        ),
        "worktree_status": checked_output(
            root,
            "git",
            "-c",
            "status.showUntrackedFiles=all",
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=none",
            environment=environment,
        ),
    }


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(64 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def environment_fingerprint(environment: dict[str, str]) -> dict[str, object]:
    values = {
        name: hashlib.sha256(value.encode()).hexdigest()
        for name, value in sorted(environment.items())
    }
    path = environment.get("PATH")
    executables = {}
    for name in ("ast-grep", "cargo", "df", "git", "mount", "rustc", "stat", "tokei"):
        executable = shutil.which(name, path=path)
        resolved = Path(executable).resolve() if executable is not None else None
        executables[name] = {
            "path": executable,
            "resolved_path": str(resolved) if resolved is not None else None,
            "sha256": sha256_file(resolved)
            if resolved is not None and resolved.is_file()
            else None,
        }
    python = Path(sys.executable).resolve()
    executables["python"] = {
        "path": sys.executable,
        "resolved_path": str(python),
        "sha256": sha256_file(python),
    }
    system_commands = (
        (
            ("filesystem_df", Path("/bin/df")),
            ("filesystem_mount", Path("/sbin/mount")),
        )
        if platform.system() == "Darwin"
        else (("filesystem_stat", Path("/usr/bin/stat")),)
    )
    for label, executable in system_commands:
        resolved = executable.resolve()
        executables[label] = {
            "path": str(executable),
            "resolved_path": str(resolved),
            "sha256": sha256_file(resolved),
        }
    return {"variables": values, "executables": executables}


def cargo_config_fingerprints(
    root: Path, environment: dict[str, str]
) -> dict[str, object]:
    cargo_home = environment.get("CARGO_HOME")
    if cargo_home is None and environment.get("HOME") is not None:
        cargo_home = str(Path(environment["HOME"]) / ".cargo")
    candidates = {"repository": root / ".cargo" / "config.toml"}
    if cargo_home is not None:
        candidates["user"] = Path(cargo_home) / "config.toml"
    return {
        label: {
            "present": path.is_file(),
            "sha256": sha256_file(path) if path.is_file() else None,
        }
        for label, path in candidates.items()
    }


def temporary_filesystem(environment: dict[str, str]) -> dict[str, str]:
    path = Path(environment.get("TMPDIR", "/tmp")).resolve()
    if platform.system() == "Darwin":
        mount_point = (
            checked_output(
                path,
                "/bin/df",
                "-P",
                str(path),
                environment=environment,
            )
            .splitlines()[-1]
            .split()[-1]
        )
        mount_lines = checked_output(
            path, "/sbin/mount", environment=environment
        ).splitlines()
        marker = f" on {mount_point} ("
        matching = [line for line in mount_lines if marker in line]
        if len(matching) != 1:
            raise RuntimeError("could not identify the temporary filesystem mount")
        filesystem = matching[0].split(marker, 1)[1].split(",", 1)[0].rstrip(")")
    elif platform.system() == "Linux":
        mount_point = "unrecorded"
        filesystem = checked_output(
            path,
            "/usr/bin/stat",
            "-f",
            "-c",
            "%T",
            str(path),
            environment=environment,
        )
    else:
        mount_point = "unrecorded"
        filesystem = "unrecorded"
    return {"path": str(path), "mount_point": mount_point, "filesystem": filesystem}


def metadata(
    root: Path,
    environment: dict[str, str],
    removed_environment: list[str],
    mode: str,
    base: str,
    toolchain: str,
) -> dict[str, object]:
    state = repository_state(root, environment)
    if state["worktree_status"]:
        raise RuntimeError("P0 evidence requires a clean worktree")
    subprocess.run(
        ("git", "merge-base", "--is-ancestor", base, state["head"]),
        cwd=root,
        check=True,
        env=environment,
    )
    rustc = checked_output(
        root, "rustc", f"+{toolchain}", "--version", environment=environment
    )
    cargo = checked_output(
        root, "cargo", f"+{toolchain}", "--version", environment=environment
    )
    if not rustc.startswith(f"rustc {toolchain} "):
        raise RuntimeError(f"unexpected rustc toolchain: {rustc}")
    if not cargo.startswith(f"cargo {toolchain} "):
        raise RuntimeError(f"unexpected cargo toolchain: {cargo}")
    return {
        "schema_version": 2,
        "kind": mode,
        "generated_at_utc": dt.datetime.now(dt.UTC).isoformat(),
        "base": base,
        "base_commit": checked_output(
            root, "git", "rev-parse", base, environment=environment
        ),
        **state,
        "platform_system": platform.system(),
        "platform": platform.platform(),
        "python": platform.python_version(),
        "rustc": rustc,
        "cargo": cargo,
        "environment": {
            "CARGO_INCREMENTAL": environment["CARGO_INCREMENTAL"],
            "CARGO_NET_OFFLINE": environment["CARGO_NET_OFFLINE"],
            "CARGO_TARGET_DIR": environment["CARGO_TARGET_DIR"],
            "CARGO_TERM_COLOR": environment["CARGO_TERM_COLOR"],
            "sanitized_variable_names": sorted(environment),
            "removed_ambient_variables": removed_environment,
        },
        "environment_fingerprint": environment_fingerprint(environment),
        "cargo_config": cargo_config_fingerprints(root, environment),
        "temporary_filesystem": temporary_filesystem(environment),
        "logical_cpu_count": os.cpu_count(),
    }


def require_macos_apfs_distribution_host(evidence: dict[str, object]) -> None:
    temporary = evidence.get("temporary_filesystem")
    filesystem = temporary.get("filesystem") if isinstance(temporary, dict) else None
    if evidence.get("platform_system") != "Darwin" or filesystem != "apfs":
        raise RuntimeError("P0 macOS race distributions require Darwin on APFS")


def output_digest(output: str) -> dict[str, object]:
    encoded = output.encode()
    return {
        "bytes": len(encoded),
        "sha256": hashlib.sha256(encoded).hexdigest(),
    }


def test_summary_lines(stdout: str, stderr: str) -> list[str]:
    return [
        line.strip()
        for line in (*stdout.splitlines(), *stderr.splitlines())
        if line.strip().startswith("test result:")
    ]


def test_counts(summaries: list[str]) -> dict[str, int]:
    counts = {"passed": 0, "failed": 0, "ignored": 0}
    pattern = re.compile(
        r"test result: .*? (?P<passed>\d+) passed; (?P<failed>\d+) failed; "
        r"(?P<ignored>\d+) ignored;"
    )
    for summary in summaries:
        match = pattern.search(summary)
        if match is not None:
            for name in counts:
                counts[name] += int(match.group(name))
    return counts


def test_names(stdout: str, stderr: str) -> list[str]:
    pattern = re.compile(r"^test (?P<name>.+) \.\.\. (?:ok|FAILED|ignored)$")
    return [
        match.group("name")
        for line in (*stdout.splitlines(), *stderr.splitlines())
        if (match := pattern.match(line.strip())) is not None
    ]


def run_once(root: Path, environment: dict[str, str], case: Case) -> dict[str, object]:
    started = time.monotonic()
    result = subprocess.run(
        case.command,
        cwd=root,
        env=environment,
        capture_output=True,
        text=True,
    )
    record: dict[str, object] = {
        "exit_code": result.returncode,
        "duration_ms": round((time.monotonic() - started) * 1000),
        "stdout": output_digest(result.stdout),
        "stderr": output_digest(result.stderr),
    }
    summaries = test_summary_lines(result.stdout, result.stderr)
    counts = test_counts(summaries)
    names = test_names(result.stdout, result.stderr)
    if summaries:
        record["test_summaries"] = summaries
        record["test_counts"] = counts
    if names:
        record["test_names"] = names
    contract_failure = None
    if case.expected_tests is not None and counts != {
        "passed": case.expected_tests,
        "failed": 0,
        "ignored": 0,
    }:
        contract_failure = (
            f"expected exactly {case.expected_tests} passing tests, observed {counts}"
        )
    elif case.minimum_tests > 0 and counts["passed"] < case.minimum_tests:
        contract_failure = (
            f"expected at least {case.minimum_tests} passing tests, observed {counts}"
        )
    if case.expected_test_names and tuple(sorted(names)) != tuple(
        sorted(case.expected_test_names)
    ):
        contract_failure = f"expected test identities {case.expected_test_names}, observed {tuple(names)}"
    if contract_failure is not None:
        record["contract_failure"] = contract_failure
    record["passed"] = result.returncode == 0 and contract_failure is None
    if not record["passed"]:
        sys.stderr.write(result.stdout)
        sys.stderr.write(result.stderr)
        if contract_failure is not None:
            sys.stderr.write(f"evidence contract failure: {contract_failure}\n")
    return record


def run_cases(
    root: Path,
    environment: dict[str, str],
    cases: list[Case],
) -> list[dict[str, object]]:
    records = []
    for case in cases:
        runs = [run_once(root, environment, case) for _ in range(case.runs)]
        records.append(
            {
                "id": case.case_id,
                "group": case.group,
                "command": list(case.command),
                "runs": case.runs,
                "expected_tests": case.expected_tests,
                "minimum_tests": case.minimum_tests,
                "expected_test_names": list(case.expected_test_names),
                "passed": sum(bool(run["passed"]) for run in runs),
                "failed": sum(not bool(run["passed"]) for run in runs),
                "observations": runs,
            }
        )
    return records
