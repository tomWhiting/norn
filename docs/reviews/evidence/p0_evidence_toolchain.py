"""Path-free executable and pinned Rust toolchain provenance."""

from __future__ import annotations

import platform
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Final

from p0_evidence_io import sha256_file


EXECUTABLE_NAMES: Final = (
    "ast-grep",
    "cargo",
    "filesystem_df",
    "filesystem_mount",
    "filesystem_stat",
    "git",
    "python",
    "rustc",
    "rustup",
    "tokei",
)


def pinned_binary(environment: dict[str, str], toolchain: str, executable: str) -> Path:
    """Resolve the actual toolchain binary, never the rustup proxy."""
    rustup = shutil.which("rustup", path=environment.get("PATH"))
    if rustup is None:
        raise RuntimeError("rustup is required for pinned evidence")
    result = subprocess.run(
        (rustup, "which", "--toolchain", toolchain, executable),
        check=True,
        capture_output=True,
        text=True,
        env=environment,
    )
    resolved = Path(result.stdout.strip()).resolve()
    if not resolved.is_file():
        raise RuntimeError(f"rustup returned no pinned {executable} binary")
    return resolved


def pinned_versions(
    root: Path, environment: dict[str, str], toolchain: str
) -> dict[str, str]:
    versions = {}
    for name in ("cargo", "rustc"):
        result = subprocess.run(
            (str(pinned_binary(environment, toolchain, name)), "--version"),
            cwd=root,
            check=True,
            capture_output=True,
            text=True,
            env=environment,
        )
        version = result.stdout.strip()
        if not version.startswith(f"{name} {toolchain} "):
            raise RuntimeError(f"unexpected {name} toolchain: {version}")
        versions[name] = version
    return versions


def environment_fingerprint(
    environment: dict[str, str], toolchain: str
) -> dict[str, object]:
    """Return hash-only identities, including actual pinned compiler binaries."""
    path = environment.get("PATH")
    executables = {
        name: {"sha256": _resolved_digest(shutil.which(name, path=path))}
        for name in ("ast-grep", "git", "rustup", "tokei")
    }
    for name in ("cargo", "rustc"):
        executables[name] = {
            "sha256": sha256_file(pinned_binary(environment, toolchain, name))
        }
    executables["python"] = {"sha256": sha256_file(Path(sys.executable).resolve())}
    system_commands = {
        "filesystem_df": Path("/bin/df") if platform.system() == "Darwin" else None,
        "filesystem_mount": (
            Path("/sbin/mount") if platform.system() == "Darwin" else None
        ),
        "filesystem_stat": (
            Path("/usr/bin/stat") if platform.system() == "Linux" else None
        ),
    }
    for label, executable in system_commands.items():
        executables[label] = {"sha256": _resolved_digest(executable)}
    if tuple(sorted(executables)) != tuple(sorted(EXECUTABLE_NAMES)):
        raise RuntimeError("executable fingerprint inventory changed")
    return {"executables": executables}


def _resolved_digest(executable: str | Path | None) -> str | None:
    if executable is None:
        return None
    resolved = Path(executable).resolve()
    return sha256_file(resolved) if resolved.is_file() else None
