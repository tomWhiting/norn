#!/usr/bin/env python3
"""Verify the NS0 inventory against pinned committed repository objects."""

from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
from pathlib import Path
from typing import Any


def run_git(repo: Path, *args: str, allow_no_match: bool = False) -> str:
    command = ["git", "-C", str(repo), *args]
    result = subprocess.run(command, check=False, capture_output=True, text=True)
    accepted = {0, 1} if allow_no_match else {0}
    if result.returncode not in accepted:
        detail = result.stderr.strip() or result.stdout.strip()
        raise RuntimeError(f"{' '.join(command)} failed: {detail}")
    return result.stdout


def source_paths(repo: Path, revision: str, exclusions: list[str]) -> list[str]:
    import re

    excluded = [re.compile(pattern) for pattern in exclusions]
    output = run_git(repo, "ls-tree", "-r", "--name-only", revision)
    paths = []
    for path in output.splitlines():
        if not path.endswith((".rs", ".ts", ".tsx")):
            continue
        if any(pattern.search(path) for pattern in excluded):
            continue
        paths.append(path)
    return sorted(paths)


def digest_lines(lines: list[str]) -> str:
    material = "\n".join(sorted(lines)).encode("utf-8")
    return hashlib.sha256(material).hexdigest()


def grep_matches(
    repo: Path,
    revision: str,
    pattern: str,
    selected_paths: set[str],
) -> list[str]:
    output = run_git(
        repo,
        "grep",
        "-n",
        "-I",
        "-E",
        pattern,
        revision,
        "--",
        "*.rs",
        "*.ts",
        "*.tsx",
        allow_no_match=True,
    )
    prefix = f"{revision}:"
    matches = []
    for line in output.splitlines():
        candidate = line[len(prefix) :] if line.startswith(prefix) else line
        path, separator, remainder = candidate.partition(":")
        if separator and path in selected_paths:
            matches.append(f"{path}:{remainder}")
    return sorted(matches)


def verify_record(repo: Path, revision: str, record: dict[str, Any]) -> list[str]:
    content = run_git(repo, "show", f"{revision}:{record['path']}")
    errors = []
    for needle in record["needles"]:
        count = content.count(needle)
        expected = int(record.get("occurrences", {}).get(needle, 1))
        if count != expected:
            errors.append(
                f"{record['id']}: expected {expected} occurrence(s) of {needle!r}, got {count}"
            )
    if record["disposition"] == "unresolved":
        errors.append(f"{record['id']}: unresolved disposition is forbidden")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--ablative-root", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
    manifest_repo = Path(
        run_git(args.manifest.parent, "rev-parse", "--show-toplevel").strip()
    )
    common_dir = Path(run_git(manifest_repo, "rev-parse", "--git-common-dir").strip())
    if not common_dir.is_absolute():
        common_dir = manifest_repo / common_dir
    ablative_root = args.ablative_root or common_dir.resolve().parent.parent
    repositories = {entry["name"]: entry for entry in manifest["repositories"]}
    records_by_repo: dict[str, list[dict[str, Any]]] = {}
    for record in manifest["records"]:
        records_by_repo.setdefault(record["repository"], []).append(record)

    errors: list[str] = []
    repository_reports = []
    query_reports = []
    negative_reports = []

    for name, entry in repositories.items():
        repo = ablative_root / entry["directory"]
        revision = entry["revision"]
        try:
            run_git(repo, "cat-file", "-e", f"{revision}^{{commit}}")
            head = run_git(repo, "rev-parse", "HEAD").strip()
            paths = source_paths(
                repo, revision, manifest["source_selection"]["exclusions"]
            )
            selected = set(paths)
            repository_reports.append(
                {
                    "name": name,
                    "revision": revision,
                    "head": head,
                    "head_matches_pin": head == revision,
                    "head_match_required": entry.get("require_head_match", False),
                    "pin_verified": True,
                    "source_path_count": len(paths),
                    "source_path_sha256": digest_lines(paths),
                }
            )
            if entry.get("require_head_match", False) and head != revision:
                errors.append(
                    f"{name}: checked-out HEAD {head} does not match pinned revision {revision}"
                )

            for query in manifest["discovery_queries"]:
                matches = grep_matches(repo, revision, query["pattern"], selected)
                query_reports.append(
                    {
                        "repository": name,
                        "axis": query["axis"],
                        "match_count": len(matches),
                        "match_sha256": digest_lines(matches),
                    }
                )

            for record in records_by_repo.get(name, []):
                if record["path"] not in selected:
                    errors.append(
                        f"{record['id']}: record path is outside selected source: {record['path']}"
                    )
                else:
                    errors.extend(verify_record(repo, revision, record))

            for assertion in manifest["negative_assertions"]:
                if assertion["repository"] != name:
                    continue
                matches = grep_matches(repo, revision, assertion["pattern"], selected)
                negative_reports.append(
                    {
                        "id": assertion["id"],
                        "repository": name,
                        "match_count": len(matches),
                    }
                )
                if matches:
                    errors.append(
                        f"{assertion['id']}: forbidden production-source match: {matches[0]}"
                    )
        except (OSError, RuntimeError, KeyError, ValueError) as error:
            errors.append(f"{name}: {error}")

    unknown_repositories = sorted(set(records_by_repo) - set(repositories))
    if unknown_repositories:
        errors.append(f"records name unknown repositories: {unknown_repositories}")

    report = {
        "format": 1,
        "manifest": str(args.manifest),
        "manifest_sha256": hashlib.sha256(args.manifest.read_bytes()).hexdigest(),
        "passed": not errors,
        "repository_count": len(repository_reports),
        "record_count": len(manifest["records"]),
        "negative_assertion_count": len(negative_reports),
        "repositories": repository_reports,
        "discovery": query_reports,
        "negative_assertions": negative_reports,
        "errors": errors,
    }
    rendered = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.output is None:
        sys.stdout.write(rendered)
    else:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    return 0 if not errors else 1


if __name__ == "__main__":
    raise SystemExit(main())
