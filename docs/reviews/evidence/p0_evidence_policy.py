"""Validation for the independently generated P0 policy artifact."""

import hashlib
import subprocess
import tempfile
from pathlib import Path
from typing import Final

import p0_evidence_support as support


POLICY_SCHEMA_VERSION: Final = 1
POLICY_MATCH_CATEGORIES: Final = frozenset(
    {
        "allow_or_expect_attr",
        "empty_cfg_any",
        "expect",
        "ignore_attr",
        "lint_cli_suppression",
        "marker",
        "panic",
        "todo_macro",
        "unimplemented",
        "unwrap",
    }
)
POLICY_METHOD_FIELDS: Final = frozenset(
    {
        "added_line_policy",
        "artifact_candidates",
        "cfg_policy",
        "counter",
        "parser",
        "rule",
    }
)


def plain_integer(value: object) -> bool:
    return type(value) is int


def independently_reproduces(
    path: Path,
    root: Path,
    expected_head: str,
    expected_base: str,
    python_executable: str,
) -> bool:
    try:
        retained = support.strict_json_loads(path.read_bytes())
        with tempfile.TemporaryDirectory(prefix="norn-p0-policy-reproduction-") as temp:
            reproduced_path = Path(temp) / "policy.json"
            result = subprocess.run(
                (
                    python_executable,
                    "-B",
                    "-I",
                    str(root / "docs/reviews/evidence/run_p0_policy_evidence.py"),
                    "--base",
                    expected_base,
                    "--head",
                    expected_head,
                    "--output",
                    str(reproduced_path),
                ),
                cwd=root,
                check=False,
                capture_output=True,
                text=True,
            )
            if result.returncode != 0:
                return False
            reproduced = support.strict_json_loads(reproduced_path.read_bytes())
    except (OSError, UnicodeDecodeError, ValueError):
        return False
    return retained == reproduced


def bind_policy_artifact(
    path: Path, root: Path, expected_head: str, expected_base: str
) -> tuple[dict[str, object], bool]:
    try:
        raw = path.read_bytes()
        policy = support.strict_json_loads(raw)
    except (OSError, UnicodeDecodeError, ValueError) as error:
        return {"error": str(error)}, False
    try:
        changed = subprocess.run(
            (
                "git",
                "diff",
                "--name-only",
                f"{expected_base}...{expected_head}",
                "--",
                "*.rs",
            ),
            cwd=root,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.splitlines()
    except (OSError, subprocess.CalledProcessError) as error:
        return {"error": str(error)}, False
    added_line_matches = policy.get("added_line_policy_matches")
    files = policy.get("files")
    method = policy.get("method")
    changed_count = policy.get("changed_rust_file_count")
    file_records_valid = isinstance(files, list) and all(
        valid_file_record(record) for record in files
    )
    file_names = [record["file"] for record in files] if file_records_valid else []
    artifact_candidates = policy.get("artifact_writer_candidates")
    contract_passed = (
        plain_integer(policy.get("schema_version"))
        and policy.get("schema_version") == POLICY_SCHEMA_VERSION
        and policy.get("head") == expected_head
        and policy.get("base") == expected_base
        and policy.get("policy_passed") is True
        and policy.get("over_500") == []
        and policy.get("thin_entrypoint_violations") == []
        and isinstance(added_line_matches, dict)
        and set(added_line_matches) == POLICY_MATCH_CATEGORIES
        and all(not matches for matches in added_line_matches.values())
        and isinstance(method, dict)
        and set(method) == POLICY_METHOD_FIELDS
        and all(isinstance(value, str) and value for value in method.values())
        and file_records_valid
        and bool(changed)
        and sorted(file_names) == sorted(changed)
        and len(set(file_names)) == len(file_names)
        and plain_integer(changed_count)
        and changed_count > 0
        and changed_count == len(files)
        and plain_integer(policy.get("test_only_file_count"))
        and policy.get("test_only_file_count")
        == sum(record["production_code_lines"] == 0 for record in files)
        and isinstance(artifact_candidates, list)
        and bool(artifact_candidates)
        and all(valid_artifact_candidate(record) for record in artifact_candidates)
    )
    return {
        "file_name": path.name,
        "sha256": hashlib.sha256(raw).hexdigest(),
        "schema_version": policy.get("schema_version"),
        "head": policy.get("head"),
        "policy_passed": policy.get("policy_passed"),
    }, contract_passed


def valid_file_record(record: object) -> bool:
    if not isinstance(record, dict):
        return False
    file_name = record.get("file")
    physical = record.get("physical_lines")
    production = record.get("production_code_lines")
    ranges = record.get("removed_test_ranges")
    entrypoint_limit = (
        200
        if isinstance(file_name, str) and Path(file_name).name in {"lib.rs", "main.rs"}
        else 500
    )
    return (
        isinstance(file_name, str)
        and bool(file_name)
        and not Path(file_name).is_absolute()
        and plain_integer(physical)
        and physical > 0
        and plain_integer(production)
        and 0 <= production <= entrypoint_limit
        and isinstance(ranges, list)
        and all(valid_removed_range(item) for item in ranges)
    )


def valid_removed_range(item: object) -> bool:
    return (
        isinstance(item, dict)
        and set(item) == {"start", "end"}
        and plain_integer(item["start"])
        and plain_integer(item["end"])
        and 0 <= item["start"] <= item["end"]
    )


def valid_artifact_candidate(record: object) -> bool:
    return (
        isinstance(record, dict)
        and set(record) == {"file", "line", "text"}
        and isinstance(record["file"], str)
        and bool(record["file"])
        and plain_integer(record["line"])
        and record["line"] > 0
        and isinstance(record["text"], str)
        and bool(record["text"])
    )
