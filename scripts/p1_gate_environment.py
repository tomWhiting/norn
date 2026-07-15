"""Private directories, scrubbed environments, and closed tools for P1."""

import hashlib
import os
import pwd
import stat
import subprocess
import sys
from pathlib import Path
from typing import Any

DARWIN_SDK_CANDIDATES = (
    (
        "macos-xcode-default",
        "/Applications/Xcode.app/Contents/Developer/Platforms/MacOSX.platform/Developer/SDKs/MacOSX.sdk",
    ),
    ("macos-command-line-tools", "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk"),
)
TOOLCHAIN_COMPONENTS = (
    "cargo",
    "cargo-clippy",
    "cargo-fmt",
    "clippy-driver",
    "rustc",
    "rustdoc",
    "rustfmt",
)
SUPPORT_IDS = (
    "p1-added-line-audit",
    "p1-redaction-check",
    "p1-distributions",
    "p1-gate-self-check",
)


class EnvironmentError(Exception):
    """A tool, permission, or environment isolation failure."""


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def private_umask() -> None:
    os.umask(0o077)


def trusted_home() -> Path:
    try:
        path = Path(pwd.getpwuid(os.getuid()).pw_dir).resolve(strict=True)
    except (KeyError, OSError) as error:
        raise EnvironmentError(
            "cannot resolve the operating-system account home"
        ) from error
    if not path.is_dir():
        raise EnvironmentError("operating-system account home is not a directory")
    return path


def validate_launcher_environment(root: Path) -> None:
    allowed = {"LANG", "LC_ALL", "PYTHONDONTWRITEBYTECODE", "TMPDIR", "TZ"}
    if os.uname().sysname == "Darwin":
        allowed.add("SDKROOT")
    if set(os.environ) != allowed:
        raise EnvironmentError("launcher did not provide the closed gate environment")
    required = {
        "LANG": "C",
        "LC_ALL": "C",
        "PYTHONDONTWRITEBYTECODE": "1",
        "TZ": "UTC",
    }
    if any(os.environ.get(name) != value for name, value in required.items()):
        raise EnvironmentError("launcher environment values are invalid")
    expected_tmp = root / "target/p1-gate/launcher-tmp"
    if os.environ.get("TMPDIR") != str(expected_tmp):
        raise EnvironmentError("launcher temporary directory is invalid")


def selected_sdk() -> tuple[str | None, str | None]:
    if os.uname().sysname != "Darwin":
        if "SDKROOT" in os.environ:
            raise EnvironmentError("SDKROOT is forbidden outside macOS")
        return None, None
    selected = next(
        (
            (identifier, path)
            for identifier, path in DARWIN_SDK_CANDIDATES
            if Path(path).is_dir()
        ),
        None,
    )
    if selected is None:
        raise EnvironmentError("no approved macOS SDK is available")
    if os.environ.get("SDKROOT") != selected[1]:
        raise EnvironmentError("launcher did not select the approved macOS SDK")
    return selected


def ensure_private_directory(root: Path, path: Path) -> None:
    try:
        relative = path.relative_to(root)
    except ValueError as error:
        raise EnvironmentError(
            "private gate directory escapes the repository"
        ) from error
    current = root
    for index, component in enumerate(relative.parts):
        current /= component
        if os.path.lexists(current):
            if current.is_symlink() or not current.is_dir():
                raise EnvironmentError(
                    f"gate directory is not a real directory: {current.relative_to(root)}"
                )
        else:
            current.mkdir(mode=0o700)
        if index >= 1 and relative.parts[0] == "target":
            current.chmod(0o700)


def regular_tool(path: Path, label: str) -> Path:
    try:
        resolved = path.resolve(strict=True)
        info = resolved.stat()
    except OSError as error:
        raise EnvironmentError(f"cannot resolve required tool: {label}") from error
    if (
        not stat.S_ISREG(info.st_mode)
        or stat.S_IMODE(info.st_mode) & 0o022
        or not os.access(resolved, os.X_OK)
    ):
        raise EnvironmentError(
            f"required tool is not a non-writable regular file: {label}"
        )
    return resolved


def discover_rust_tools(home: Path) -> tuple[dict[str, Path], Path]:
    rustup = regular_tool(home / ".cargo/bin/rustup", "rustup")
    environment = {
        "HOME": str(home),
        "LANG": "C",
        "LC_ALL": "C",
        "PATH": "/usr/bin:/bin",
        "RUSTUP_HOME": str(home / ".rustup"),
        "TZ": "UTC",
    }
    result = subprocess.run(
        [str(rustup), "which", "cargo"],
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        raise EnvironmentError("rustup could not resolve the active cargo toolchain")
    try:
        cargo = regular_tool(Path(result.stdout.decode("utf-8").strip()), "cargo")
    except UnicodeError as error:
        raise EnvironmentError("rustup returned an invalid cargo path") from error
    toolchain_bin = cargo.parent
    tools = {
        name: regular_tool(toolchain_bin / name, name) for name in TOOLCHAIN_COMPONENTS
    }
    if tools["cargo"] != cargo:
        raise EnvironmentError("rustup cargo resolution left the selected toolchain")
    tools["rustup"] = rustup
    return tools, toolchain_bin


def resolve_tools(root: Path, pins: dict[str, dict[str, str]]) -> dict[str, Any]:
    home = trusted_home()
    rust_tools, toolchain_bin = discover_rust_tools(home)
    paths: dict[str, Path] = {
        **rust_tools,
        "git": regular_tool(Path("/usr/bin/git"), "git"),
        "python": regular_tool(Path(sys.executable), "python"),
        "shell": regular_tool(Path("/bin/sh"), "shell"),
    }
    pin_by_path = {entry["path"]: entry for entry in pins.values()}
    for identifier in SUPPORT_IDS:
        relative = f"scripts/{identifier}"
        pin = pin_by_path.get(relative)
        if pin is None:
            raise EnvironmentError(f"support script is not pinned: {identifier}")
        path = regular_tool(root / relative, identifier)
        if sha256_file(path) != pin["sha256"]:
            raise EnvironmentError(
                f"support script hash differs from manifest: {identifier}"
            )
        paths[identifier] = path
    records = [
        {"id": identifier, "sha256": sha256_file(path)}
        for identifier, path in sorted(paths.items())
    ]
    return {
        "home": home,
        "paths": paths,
        "records": records,
        "toolchain_bin": toolchain_bin,
    }


def command_prefix(token: str, tools: dict[str, Any]) -> list[str]:
    if token in {"@cargo", "@git", "@rustc"}:
        identifier = token[1:]
    elif token.startswith("@support:"):
        identifier = token.removeprefix("@support:")
    else:
        raise EnvironmentError("command contains an unknown logical executable")
    path = tools["paths"].get(identifier)
    if path is None:
        raise EnvironmentError("command tool was not resolved")
    return [str(path)]


def verify_tools(tools: dict[str, Any]) -> None:
    expected = {record["id"]: record["sha256"] for record in tools["records"]}
    if set(expected) != set(tools["paths"]):
        raise EnvironmentError("tool record inventory changed")
    for identifier, path in tools["paths"].items():
        if sha256_file(path) != expected[identifier]:
            raise EnvironmentError(f"tool changed during gate: {identifier}")


def prepare_cache_bridges(root: Path, home: Path) -> tuple[Path, list[str]]:
    cargo_home = root / "target/p1-gate/cargo-home"
    ensure_private_directory(root, cargo_home)
    for forbidden in ("config", "config.toml", "credentials", "credentials.toml"):
        if os.path.lexists(cargo_home / forbidden):
            raise EnvironmentError(
                "isolated Cargo home contains forbidden configuration or credentials"
            )
    bridges: list[str] = []
    for name in ("git", "registry"):
        source = home / ".cargo" / name
        destination = cargo_home / name
        if not source.is_dir() or source.is_symlink():
            continue
        if os.path.lexists(destination):
            if not destination.is_symlink() or destination.resolve(
                strict=True
            ) != source.resolve(strict=True):
                raise EnvironmentError(f"Cargo cache bridge is invalid: {name}")
        else:
            destination.symlink_to(source, target_is_directory=True)
        bridges.append(f"cargo-{name}-cache")
    return cargo_home, bridges


def verify_cargo_isolation(
    cargo_home: Path, home: Path, expected_bridges: list[str]
) -> None:
    for forbidden in ("config", "config.toml", "credentials", "credentials.toml"):
        if os.path.lexists(cargo_home / forbidden):
            raise EnvironmentError(
                "isolated Cargo home gained configuration or credentials"
            )
    observed: list[str] = []
    for name in ("git", "registry"):
        destination = cargo_home / name
        source = home / ".cargo" / name
        if not os.path.lexists(destination):
            continue
        if not destination.is_symlink() or destination.resolve(
            strict=True
        ) != source.resolve(strict=True):
            raise EnvironmentError(f"Cargo cache bridge changed during gate: {name}")
        observed.append(f"cargo-{name}-cache")
    if observed != expected_bridges:
        raise EnvironmentError("Cargo cache bridge inventory changed during gate")


def controlled_environment(
    root: Path,
    run_id: str,
    tools: dict[str, Any],
    sdk_path: str | None,
    sdk_id: str | None,
) -> tuple[dict[str, str], dict[str, Any]]:
    run_root = root / "target/p1-gate/runtime" / run_id
    isolated_home = run_root / "home"
    temporary = run_root / "tmp"
    target = root / "target/p1-gate/cargo-target"
    for path in (run_root, isolated_home, temporary, target):
        ensure_private_directory(root, path)
    cargo_home, bridges = prepare_cache_bridges(root, tools["home"])
    actual = {
        "CARGO_BUILD_JOBS": "1",
        "CARGO_HOME": str(cargo_home),
        "CARGO_INCREMENTAL": "0",
        "CARGO_NET_OFFLINE": "true",
        "CARGO_TARGET_DIR": str(target),
        "CARGO_TERM_COLOR": "never",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_SYSTEM": "/dev/null",
        "GIT_PAGER": "cat",
        "GIT_TERMINAL_PROMPT": "0",
        "HOME": str(isolated_home),
        "LANG": "C",
        "LC_ALL": "C",
        "NO_COLOR": "1",
        "PAGER": "cat",
        "PATH": f"{tools['toolchain_bin']}:/usr/bin:/bin",
        "PYTHONDONTWRITEBYTECODE": "1",
        "RUST_BACKTRACE": "0",
        "RUSTC": str(tools["paths"]["rustc"]),
        "RUSTDOC": str(tools["paths"]["rustdoc"]),
        "TERM": "dumb",
        "TMPDIR": str(temporary),
        "TZ": "UTC",
    }
    if sdk_path is not None:
        actual["SDKROOT"] = sdk_path

    def relative(path: Path) -> str:
        return path.relative_to(root).as_posix()

    recorded = {
        "caller_environment_inherited": [],
        "credential_environment_inherited": [],
        "cache_bridges": bridges,
        "controlled": {
            **{
                key: value
                for key, value in actual.items()
                if key
                not in {
                    "CARGO_HOME",
                    "CARGO_TARGET_DIR",
                    "HOME",
                    "PATH",
                    "RUSTC",
                    "RUSTDOC",
                    "SDKROOT",
                    "TMPDIR",
                }
            },
            "CARGO_HOME": relative(cargo_home),
            "CARGO_TARGET_DIR": relative(target),
            "HOME": relative(isolated_home),
            "PATH": ["selected-rust-toolchain", "system-usr-bin", "system-bin"],
            "RUSTC": "tool:rustc",
            "RUSTDOC": "tool:rustdoc",
            "SDKROOT": sdk_id,
            "TMPDIR": relative(temporary),
        },
    }
    return actual, recorded
