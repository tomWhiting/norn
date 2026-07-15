"""Recompute exact P1 analysis-snapshot and generated-include evidence."""

from __future__ import annotations

import hashlib
import json
import subprocess
import sys
import unicodedata
from pathlib import Path
from typing import Any

BASE_COMMIT = "2917c8ed10e7a2ec7ac9c4d7283bafbea7f6577d"
BASE_TREE = "9ae969792c53b4e1dfdc61c6d91f7fe62d3ac582"
SNAPSHOT_DOMAIN = b"norn-policy-owned-snapshot-1"
GIT_INVENTORY_DOMAIN = b"norn-policy-p1-git-tree-inventory-1"
REGISTRY_DOMAIN = b"norn-policy-p1-generated-include-technical-registry-1"
ROOT = Path(__file__).resolve().parent.parent


class EvidenceError(Exception):
    """A Git object or canonical evidence invariant failed."""


def decode_strict_json(text: str) -> Any:
    """Decode JSON while rejecting duplicate object keys at every depth."""

    def object_from_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise EvidenceError("evidence JSON contains a duplicate object key")
            result[key] = value
        return result

    def reject_constant(_value: str) -> None:
        raise EvidenceError("evidence JSON contains a non-finite number")

    try:
        return json.loads(
            text,
            object_pairs_hook=object_from_pairs,
            parse_constant=reject_constant,
        )
    except json.JSONDecodeError as error:
        raise EvidenceError("evidence JSON is invalid") from error


def run_git(root: Path, *arguments: str) -> bytes:
    """Run one read-only Git query and return exact stdout bytes."""
    result = subprocess.run(
        ["git", "--no-replace-objects", "-C", str(root), *arguments],
        check=False,
        capture_output=True,
    )
    if result.returncode != 0:
        raise EvidenceError(result.stderr.decode("utf-8", errors="replace"))
    return result.stdout


def append_length(digest: Any, length: int) -> None:
    """Append the fixed-width unsigned framing shared with Rust."""
    if length < 0 or length >= 1 << 128:
        raise EvidenceError("identity length is outside u128")
    digest.update(length.to_bytes(16, byteorder="big"))


def append_field(digest: Any, value: bytes) -> None:
    """Append one u128-length-framed byte string."""
    append_length(digest, len(value))
    digest.update(value)


def validate_path(path: bytes) -> bytes:
    """Require the same normalized UTF-8 repository-path shape as Rust."""
    try:
        text = path.decode("utf-8", errors="strict")
    except UnicodeDecodeError as error:
        raise EvidenceError("Git tree contains a non-UTF-8 path") from error
    components = text.split("/")
    invalid = (
        not text
        or text.startswith("/")
        or (
            len(text) >= 2
            and text[0].isascii()
            and text[0].isalpha()
            and text[1] == ":"
        )
        or "\\" in text
        or any(unicodedata.category(character) == "Cc" for character in text)
        or any(component in {"", ".", ".."} for component in components)
    )
    if invalid:
        raise EvidenceError(f"Git tree contains a non-repository path: {text!r}")
    return path


def parse_tree(root: Path) -> list[tuple[bytes, int, str, str]]:
    """Read and normalize every leaf in the exact base tree."""
    resolved = (
        run_git(root, "rev-parse", f"{BASE_COMMIT}^{{tree}}").strip().decode("ascii")
    )
    if resolved != BASE_TREE:
        raise EvidenceError("base commit does not resolve to the retained tree")
    rows = run_git(root, "ls-tree", "-rz", "--full-tree", "-r", BASE_TREE)
    entries: list[tuple[bytes, int, str, str]] = []
    for row in rows.split(b"\0"):
        if not row:
            continue
        try:
            metadata, path = row.split(b"\t", maxsplit=1)
            mode_bytes, type_bytes, object_bytes = metadata.split(b" ", maxsplit=2)
            mode = mode_bytes.decode("ascii")
            object_type = type_bytes.decode("ascii")
            object_id = object_bytes.decode("ascii")
        except (ValueError, UnicodeDecodeError) as error:
            raise EvidenceError("Git tree row is malformed") from error
        if object_type != "blob":
            raise EvidenceError(f"unsupported Git object type: {object_type}")
        if mode in {"100644", "100755"}:
            kind = 0
        elif mode == "120000":
            kind = 1
        else:
            raise EvidenceError(f"unsupported Git entry mode: {mode}")
        entries.append((validate_path(path), kind, mode, object_id))
    entries.sort(key=lambda entry: entry[0])
    return entries


def read_blobs(root: Path, object_ids: list[str]) -> dict[str, bytes]:
    """Read unique blobs through one `git cat-file --batch` process."""
    unique_ids = list(dict.fromkeys(object_ids))
    request = b"".join(f"{object_id}\n".encode("ascii") for object_id in unique_ids)
    process = subprocess.run(
        ["git", "--no-replace-objects", "-C", str(root), "cat-file", "--batch"],
        input=request,
        check=False,
        capture_output=True,
    )
    if process.returncode != 0:
        raise EvidenceError(process.stderr.decode("utf-8", errors="replace"))
    cursor = 0
    objects: dict[str, bytes] = {}
    for expected_id in unique_ids:
        header_end = process.stdout.find(b"\n", cursor)
        if header_end < 0:
            raise EvidenceError("cat-file header is missing")
        header = process.stdout[cursor:header_end].split()
        if len(header) != 3:
            raise EvidenceError("cat-file header is malformed")
        actual_id, object_type, size_bytes = header
        if actual_id.decode("ascii") != expected_id or object_type != b"blob":
            raise EvidenceError("cat-file returned a different object")
        try:
            size = int(size_bytes)
        except ValueError as error:
            raise EvidenceError("cat-file size is malformed") from error
        content_start = header_end + 1
        content_end = content_start + size
        if process.stdout[content_end : content_end + 1] != b"\n":
            raise EvidenceError("cat-file content framing is malformed")
        objects[expected_id] = process.stdout[content_start:content_end]
        cursor = content_end + 1
    if cursor != len(process.stdout):
        raise EvidenceError("cat-file returned unrequested bytes")
    return objects


def snapshot_identity(
    entries: list[tuple[bytes, int, str, str]],
    objects: dict[str, bytes],
) -> str:
    """Hash the exact semantic analysis projection."""
    if entries != sorted(entries, key=lambda entry: entry[0]):
        raise EvidenceError("analysis snapshot entries are not path sorted")
    digest = hashlib.sha256()
    append_field(digest, SNAPSHOT_DOMAIN)
    append_length(digest, len(entries))
    for path, kind, _mode, object_id in entries:
        append_field(digest, path)
        digest.update(bytes([kind]))
        append_field(digest, objects[object_id])
    return digest.hexdigest()


def git_inventory_identity(entries: list[tuple[bytes, int, str, str]]) -> str:
    """Hash every exact base leaf path, Git mode, and blob object ID."""
    if entries != sorted(entries, key=lambda entry: entry[0]):
        raise EvidenceError("Git inventory entries are not path sorted")
    digest = hashlib.sha256()
    append_field(digest, GIT_INVENTORY_DOMAIN)
    append_length(digest, len(entries))
    for path, _kind, mode, object_id in entries:
        append_field(digest, path)
        append_field(digest, mode.encode("ascii"))
        append_field(digest, object_id.encode("ascii"))
    return digest.hexdigest()


def technical_registry(
    entries: list[tuple[bytes, int, str, str]],
    objects: dict[str, bytes],
) -> dict[str, Any]:
    """Build the sole exact P1 generated-include registration."""
    object_by_path = {
        path: objects[object_id] for path, _kind, _mode, object_id in entries
    }
    try:
        source = object_by_path[b"crates/norn/src/model_catalog.rs"]
        build = object_by_path[b"crates/norn/build.rs"]
        models = object_by_path[b"assets/models.json"]
    except KeyError as error:
        raise EvidenceError("generated-include authority input is absent") from error
    invocation = b'include!(concat!(env!("OUT_DIR"), "/model_catalog_generated.rs"))'
    callsite_start = source.find(invocation)
    enclosing_start = source.find(b"mod generated {")
    enclosing_close = source.find(b"\n}\n\n#[cfg(test)]", enclosing_start)
    if (
        min(callsite_start, enclosing_start, enclosing_close) < 0
        or source.count(invocation) != 1
        or source.count(b"mod generated {") != 1
    ):
        raise EvidenceError("generated-include source shape has drifted")
    normalized_invocation = (
        b'include!(concat!(env!("OUT_DIR"),"/model_catalog_generated.rs"))'
    )
    return {
        "schema_version": 1,
        "entries": [
            {
                "source": "crates/norn/src/model_catalog.rs",
                "callsite": {
                    "start": callsite_start,
                    "end": callsite_start + len(invocation),
                },
                "enclosing_item": {
                    "start": enclosing_start,
                    "end": enclosing_close + 2,
                },
                "invocation_digest": hashlib.sha256(normalized_invocation).hexdigest(),
                "target": {
                    "package": "norn",
                    "package_root": "crates/norn",
                    "kind": "library",
                    "name": "norn",
                    "root": "crates/norn/src/lib.rs",
                },
                "generator": {
                    "path": "crates/norn/build.rs",
                    "digest": hashlib.sha256(build).hexdigest(),
                },
                "inputs": [
                    {
                        "path": "assets/models.json",
                        "digest": hashlib.sha256(models).hexdigest(),
                    }
                ],
                "output_basename": "model_catalog_generated.rs",
            }
        ],
    }


def registry_identity(registry: dict[str, Any]) -> str:
    """Hash canonical JSON for every executable registry field."""
    canonical = json.dumps(
        registry,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    digest = hashlib.sha256()
    append_field(digest, REGISTRY_DOMAIN)
    append_field(digest, canonical)
    return digest.hexdigest()


def build_evidence(root: Path) -> dict[str, Any]:
    """Recompute the complete retained evidence document."""
    entries = parse_tree(root)
    objects = read_blobs(root, [entry[3] for entry in entries])
    registry = technical_registry(entries, objects)
    mode_counts: dict[str, int] = {}
    for _path, _kind, mode, _object_id in entries:
        mode_counts[mode] = mode_counts.get(mode, 0) + 1
    return {
        "schema_version": 1,
        "commit": BASE_COMMIT,
        "tree": BASE_TREE,
        "algorithm": "sha256",
        "domain": SNAPSHOT_DOMAIN.decode("ascii"),
        "framing": "u128-be length; domain; entry count; sorted path, one-byte kind, exact bytes",
        "entry_count": len(entries),
        "mode_counts": dict(sorted(mode_counts.items())),
        "analysis_projection": {
            "regular_git_modes": ["100644", "100755"],
            "regular_kind_tag": 0,
            "symlink_git_mode": "120000",
            "symlink_kind_tag": 1,
            "other_kind_tag": 2,
            "executable_mode_collapsed": True,
        },
        "analysis_snapshot_identity": snapshot_identity(entries, objects),
        "git_inventory_domain": GIT_INVENTORY_DOMAIN.decode("ascii"),
        "git_inventory_identity": git_inventory_identity(entries),
        "generated_include_registry_domain": REGISTRY_DOMAIN.decode("ascii"),
        "generated_include_registry_framing": (
            "u128-be length-framed domain and canonical JSON"
        ),
        "generated_include_registry": registry,
        "generated_include_registry_identity": registry_identity(registry),
    }


def main() -> int:
    """Print deterministic evidence without writing repository or temporary files."""
    if sys.version_info < (3, 11):
        print("p1_origin_evidence.py requires Python 3.11+", file=sys.stderr)
        return 2
    try:
        evidence = build_evidence(ROOT)
    except EvidenceError as error:
        print(str(error), file=sys.stderr)
        return 1
    print(json.dumps(evidence, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
