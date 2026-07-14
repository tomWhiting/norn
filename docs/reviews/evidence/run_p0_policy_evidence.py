#!/usr/bin/env python3
"""Generate syntax-aware P0 production-LOC and added-line policy evidence."""

import argparse
import importlib
import json
import re
import subprocess
import tempfile
from pathlib import Path
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
artifact_writers = importlib.import_module("p0_artifact_writers")
module_shape = importlib.import_module("p0_module_shape")
module_references = importlib.import_module("p0_module_references")
paths = importlib.import_module("p0_evidence_paths")
policy_rust = importlib.import_module("p0_policy_rust")
rust_literals = importlib.import_module("p0_rust_literals")
ast_matches = policy_rust.ast_matches
attribute_matches = policy_rust.attribute_matches
strip_ranges = policy_rust.strip_ranges
test_only_ranges = policy_rust.test_only_ranges
FORBIDDEN = {
    "unwrap": re.compile(r"\.(?:unwrap|unwrap_err)\s*\("),
    "expect": re.compile(r"\.(?:expect|expect_err)\s*\("),
    "panic": re.compile(r"\bpanic!\s*\("),
    "todo_macro": re.compile(r"\btodo!\s*\("),
    "unimplemented": re.compile(r"\bunimplemented!\s*\("),
    "allow_or_expect_attr": re.compile(r"#\s*\[\s*(?:allow|expect)\s*\("),
    "ignore_attr": re.compile(r"#\s*\[\s*ignore(?:\s|\]|\()"),
    "empty_cfg_any": re.compile(r"#\s*\[\s*cfg\s*\(\s*any\s*\(\s*\)"),
    "lint_cli_suppression": re.compile(r"(?:^|\s)-A(?:\s|clippy::|warnings)"),
    "marker": re.compile(r"(?:^|\s)(?://+|/\*+|\*+)\s*[^\n]*\b(?:TODO|FIXME|HACK)\b"),
}


def command(*args: str, cwd: Path) -> str:
    result = subprocess.run(
        args,
        cwd=cwd,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    return result.stdout


def changed_rust_files(repo: Path, base: str, head: str) -> list[Path]:
    raw = command(
        "git",
        "diff",
        "--name-only",
        f"{base}...{head}",
        "--",
        "*.rs",
        cwd=repo,
    )
    return [repo / line for line in raw.splitlines() if line]


def tokei_counts(root: Path, repo: Path) -> dict[str, int]:
    raw = command("tokei", "--output", "json", str(root), cwd=repo)
    payload = json.loads(raw)
    reports = payload.get("Rust", {}).get("reports", [])
    return {
        str(Path(report["name"]).resolve().relative_to(root.resolve())): report[
            "stats"
        ]["code"]
        for report in reports
    }


def added_line_evidence(repo: Path, base: str, head: str) -> dict[str, list[dict]]:
    raw = command(
        "git",
        "diff",
        "--no-ext-diff",
        "--unified=0",
        f"{base}...{head}",
        "--",
        "*.rs",
        "Cargo.toml",
        "crates/*/Cargo.toml",
        cwd=repo,
    )
    matches = {name: [] for name in FORBIDDEN}
    current_file = ""
    masked_lines: list[str] | None = None
    new_line = 0
    for line in raw.splitlines():
        if line.startswith("+++ b/"):
            current_file = line[6:]
            masked_lines = rust_literals.policy_lines(repo, current_file)
            continue
        hunk = re.match(r"@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@", line)
        if hunk:
            new_line = int(hunk.group(1))
            continue
        if line.startswith("+") and not line.startswith("+++"):
            content = line[1:]
            scanned = (
                masked_lines[new_line - 1] if masked_lines is not None else content
            )
            for name, pattern in FORBIDDEN.items():
                if pattern.search(scanned):
                    matches[name].append(
                        {
                            "file": current_file,
                            "line": new_line,
                            "text": content.strip(),
                        }
                    )
            new_line += 1
        elif line.startswith(" "):
            new_line += 1
    return matches


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="41ea210")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    repo = Path(__file__).resolve().parents[3]
    layout = paths.RepositoryTargetLayout.locate(repo)
    output = (
        layout.require_lane_path(args.output, "evidence", "policy output")
        if args.output is not None
        else None
    )
    rule = Path(__file__).with_name("p0-rust-items.yml")
    module_shape.validate_fixtures(repo, rule, ast_matches, test_only_ranges)
    fixture_references = module_references.validate_fixtures(
        repo, rule, ast_matches, attribute_matches, test_only_ranges
    )
    module_shape.validate_reference_fixtures(
        repo, rule, fixture_references, ast_matches, test_only_ranges
    )
    files = changed_rust_files(repo, args.base, args.head)
    references = module_references.discover_reference_inventory(
        repo, rule, ast_matches, attribute_matches, test_only_ranges
    )
    test_only_files = module_references.test_only_identities(repo, references)

    records: list[dict] = []
    scratch_parent = layout.target_root / "build"
    scratch_parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix="norn-p0-policy-", dir=scratch_parent
    ) as directory:
        stripped_root = Path(directory)
        for source in files:
            relative = source.relative_to(repo)
            destination = stripped_root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            path_parts = relative.parts
            integration_test = (
                len(path_parts) >= 3
                and path_parts[0] == "crates"
                and "tests" in path_parts[2:]
            )
            if (
                module_references.file_identity(source) in test_only_files
                or integration_test
            ):
                ranges = [(0, len(source.read_bytes()))]
            else:
                ranges, _modules = test_only_ranges(repo, rule, source)
            stripped = strip_ranges(source.read_text(encoding="utf-8"), ranges)
            destination.write_text(stripped, encoding="utf-8")
            records.append(
                {
                    "file": str(relative),
                    "physical_lines": len(
                        source.read_text(encoding="utf-8").splitlines()
                    ),
                    "removed_test_ranges": [
                        {"start": start, "end": end} for start, end in ranges
                    ],
                }
            )
        counts = tokei_counts(stripped_root, repo)

    for record in records:
        record["production_code_lines"] = counts.get(record["file"], 0)
    records.sort(key=lambda record: (-record["production_code_lines"], record["file"]))
    over_limit = [
        record["file"] for record in records if record["production_code_lines"] > 500
    ]
    thin_entrypoint_violations = [
        record["file"]
        for record in records
        if Path(record["file"]).name in {"lib.rs", "main.rs"}
        and record["production_code_lines"] > 200
    ]
    mod_shape_violations = module_shape.repository_violations(
        repo, rule, test_only_files, references, ast_matches, test_only_ranges
    )
    added_line_matches = added_line_evidence(repo, args.base, args.head)
    policy_passed = not (
        over_limit
        or thin_entrypoint_violations
        or mod_shape_violations
        or any(added_line_matches.values())
    )
    evidence = {
        "schema_version": 2,
        "base": args.base,
        "head": command("git", "rev-parse", args.head, cwd=repo).strip(),
        "method": {
            "parser": command("ast-grep", "--version", cwd=repo).strip(),
            "counter": command("tokei", "--version", cwd=repo).strip(),
            "rule": str(rule.relative_to(repo)),
            "cfg_policy": "remove AST items whose cfg cannot be true with test=false",
            "module_shape_policy": (
                "repository-wide AST sweep requires production mod.rs files to contain "
                "only module declarations and visibility-bearing use re-exports; "
                "a module file is test-only only when every discovered reference is "
                "test-gated; path aliases and checked-in literal include targets are "
                "grouped by file identity, while OUT_DIR-generated includes are the "
                "only dynamic exception; checked-in fixtures run on every gate"
            ),
            "added_line_policy": (
                "scan added Rust lines for prohibited calls, attributes, lint flags, "
                "and debt markers in comments; todo! and unimplemented! are separate "
                "rules, while string fixtures containing marker words are not debt"
            ),
            "artifact_candidates": (
                "repository-wide production/build-script lexical sweep for filesystem "
                "roots, opens, creates, mutations, publication, and removal; the retained "
                "candidate list is intentionally conservative and requires the adjacent "
                "manual ownership/lifetime classification"
            ),
        },
        "changed_rust_file_count": len(records),
        "test_only_file_count": sum(
            record["production_code_lines"] == 0 for record in records
        ),
        "over_500": over_limit,
        "thin_entrypoint_violations": thin_entrypoint_violations,
        "module_shape_violations": mod_shape_violations,
        "files": records,
        "added_line_policy_matches": added_line_matches,
        "policy_passed": policy_passed,
        "artifact_writer_candidates": artifact_writers.candidates(
            repo, rule, test_only_files, test_only_ranges
        ),
    }
    rendered = json.dumps(evidence, indent=2, sort_keys=True) + "\n"
    if output is not None:
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(rendered, encoding="utf-8")
    else:
        print(rendered, end="")
    return 0 if policy_passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
