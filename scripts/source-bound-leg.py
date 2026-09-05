#!/usr/bin/env python3
"""Guard one declared leg with clean-tree/HEAD witnesses, not immutable execution.

Checks before and after cannot detect an edit restored during the command. The
landing desk must compare declaration/guard digests with the reviewed commit and
exclude unreviewed writers from the run tree. This does not repair Aion globally.
"""

import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys


REFUSED = 126


class ProbeError(Exception):
    """A named source probe failed or found an unsupported checkout state."""


def git(*arguments):
    """Run a read-only Git probe without cached filesystem-monitor assumptions."""
    try:
        result = subprocess.run(
            ["git", "--no-optional-locks", "-c", "core.fsmonitor=false",
             "-c", "core.untrackedCache=false", "-c", "core.fileMode=true", *arguments],
            capture_output=True, text=True, errors="surrogateescape", check=False,
        )
    except OSError as error:
        raise ProbeError(f"git {arguments!r}: {error}") from error
    if result.returncode != 0:
        raise ProbeError(
            f"git {arguments!r} exited {result.returncode}: {result.stderr.strip()}"
        )
    return result.stdout


def tracked_blob_changes(root):
    """Compare physical bytes/modes with HEAD, independently of Git's stat cache."""
    object_format = git("rev-parse", "--show-object-format").strip()
    if object_format not in ("sha1", "sha256"):
        raise ProbeError(f"unsupported Git object format {object_format!r}")
    changes = []
    for entry in git("ls-tree", "-r", "-z", "--full-tree", "HEAD").split("\0"):
        if not entry:
            continue
        metadata, separator, name = entry.partition("\t")
        fields = metadata.split()
        if not separator or len(fields) != 3:
            raise ProbeError(f"invalid Git tree entry {entry!r}")
        mode, kind, expected = fields
        if kind != "blob" or mode not in ("100644", "100755", "120000"):
            raise ProbeError(f"unsupported tracked entry {name!r}: {mode} {kind}")
        path = root / name
        try:
            actual_mode = path.lstat().st_mode
            if mode == "120000":
                if not stat.S_ISLNK(actual_mode):
                    changes.append(f"type mismatch: {name}")
                    continue
                data = os.fsencode(os.readlink(path))
            else:
                if not stat.S_ISREG(actual_mode):
                    changes.append(f"type mismatch: {name}")
                    continue
                if bool(actual_mode & stat.S_IXUSR) != (mode == "100755"):
                    changes.append(f"executable mode mismatch: {name}")
                data = path.read_bytes()
        except OSError as error:
            raise ProbeError(f"tracked source {name!r}: {error}") from error
        digest = hashlib.new(object_format)
        digest.update(f"blob {len(data)}\0".encode("ascii"))
        digest.update(data)
        if digest.hexdigest() != expected:
            changes.append(f"blob mismatch: {name}")
    return changes


def witness():
    """Capture a HEAD and dirty entries, including staged and untracked changes."""
    root = Path(git("rev-parse", "--show-toplevel").strip()).resolve()
    if root != Path.cwd().resolve():
        raise ProbeError(f"leg must run at repository root {root}; cwd is {Path.cwd()}")
    head = git("rev-parse", "--verify", "HEAD^{commit}").strip()
    entries = git("status", "--porcelain=v1", "-z", "--untracked-files=no",
                  "--ignore-submodules=none").split("\0")
    # Use repository .gitignore files, not a user's global or .git/info excludes.
    untracked = git("ls-files", "--others", "--exclude-per-directory=.gitignore",
                    "-z").split("\0")
    flags = git("ls-files", "-v", "-z").split("\0")
    hidden = [entry for entry in flags
              if entry and (entry[0].islower() or entry[0] == "S")]
    if hidden:
        raise ProbeError(f"assume-unchanged/skip-worktree entries hide source: {hidden!r}")
    try:
        digests = {
            "declaration_sha256": hashlib.sha256((root / "gates.json").read_bytes()).hexdigest(),
            "guard_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        }
    except OSError as error:
        raise ProbeError(f"source witness digest: {error}") from error
    return {"head": head, "dirty": [entry for entry in entries if entry]
            + [f"?? {entry}" for entry in untracked if entry]
            + tracked_blob_changes(root), **digests}


def report(**fields):
    """Publish machine-readable source evidence into the leg's captured output."""
    print(json.dumps({"schema": "norn-source-bound-leg.v1", **fields}), flush=True)


def main(arguments):
    """Execute argv only after clean source probes and preserve command failures."""
    if arguments[:1] == ["--"]:
        arguments = arguments[1:]
    if not arguments:
        report(phase="before", verdict="refused", error="missing leg command", exit=REFUSED)
        return REFUSED
    try:
        before = witness()
    except ProbeError as error:
        report(phase="before", verdict="refused", error=str(error), exit=REFUSED)
        return REFUSED
    if before["dirty"]:
        report(phase="before", verdict="refused", witness=before, exit=REFUSED)
        return REFUSED
    report(phase="before", verdict="clean", witness=before, command=arguments)
    command_error = None
    try:
        result = subprocess.run(arguments, check=False)
        command_exit = result.returncode if result.returncode >= 0 else 128 - result.returncode
    except OSError as error:
        command_error = f"command {arguments!r}: {error}"
        command_exit = 127
    except KeyboardInterrupt:
        command_error = f"command {arguments!r}: interrupted"
        command_exit = 130
    try:
        after = witness()
        changed = after != before
        source_error = "HEAD, source cleanliness or source digests changed" if changed else None
    except ProbeError as error:
        after = None
        source_error = str(error)
    exit_code = command_exit or (REFUSED if source_error else 0)
    report(phase="after", verdict="green" if exit_code == 0 else "red", witness=after,
           command_exit=command_exit, command_error=command_error,
           source_error=source_error, exit=exit_code)
    return exit_code


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
