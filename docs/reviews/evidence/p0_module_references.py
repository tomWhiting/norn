"""Reference-aware classification of Rust module files for P0 policy checks."""

import json
import os
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Iterable


ByteRange = tuple[int, int]
AstMatches = Callable[[Path, Path, Path], list[dict]]
AttributeMatches = Callable[[Path, Path], list[dict]]
TestOnlyRanges = Callable[[Path, Path, Path], tuple[list[ByteRange], list[Path]]]
FileIdentity = tuple[str, int, int] | tuple[str, str]

MODULE_DECLARATION = re.compile(
    r"(?:pub(?:\([^)]*\))?\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*;\Z",
    re.DOTALL,
)
PATH_ATTRIBUTE = re.compile(r'#\[\s*path\s*=\s*"([^"]+)"\s*\]\Z')
PATH_SETTING = re.compile(r"\bpath\s*=")
INCLUDE_PREFIX = re.compile(r"include\s*!")
INCLUDE_INVOCATION = re.compile(
    r"include\s*!\s*\(\s*(?P<literal>r(?P<hashes>#{0,32})\".*\"(?P=hashes)|"
    r'"(?:\\.|[^"\\])*")\s*\)\s*;?\Z',
    re.DOTALL,
)
GENERATED_INCLUDE = re.compile(
    r'include\s*!\s*\(.*env!\s*\(\s*"OUT_DIR"\s*\).*\)\s*;?\Z',
    re.DOTALL,
)


@dataclass
class ReferenceRecord:
    path: Path
    kinds: set[str] = field(default_factory=set)


def file_identity(path: Path) -> FileIdentity:
    resolved = path.resolve(strict=True)
    status = resolved.stat()
    if status.st_ino:
        return ("inode", status.st_dev, status.st_ino)
    return ("path", os.path.normcase(str(resolved)))


def record_reference(
    inventory: dict[FileIdentity, ReferenceRecord],
    path: Path,
    kind: str,
    identity: FileIdentity | None = None,
) -> None:
    key = identity if identity is not None else file_identity(path)
    record = inventory.get(key)
    if record is None:
        record = ReferenceRecord(path=path)
        inventory[key] = record
    elif str(path) < str(record.path):
        record.path = path
    record.kinds.add(kind)


def byte_range(match: dict) -> ByteRange:
    offsets = match["range"]["byteOffset"]
    return offsets["start"], offsets["end"]


def associated_attributes(
    data: bytes, item_start: int, attributes: list[dict]
) -> list[dict]:
    """Return the contiguous outer attributes immediately before an item."""
    associated: list[dict] = []
    cursor = item_start
    for attribute in reversed(attributes):
        start, end = byte_range(attribute)
        if end > cursor:
            continue
        if data[end:cursor].strip():
            break
        associated.append(attribute)
        cursor = start
    associated.reverse()
    return associated


def resolve_module_file(source: Path, attributes: Iterable[str], item: str) -> Path:
    for attribute in attributes:
        path_match = PATH_ATTRIBUTE.fullmatch(attribute)
        if path_match is not None:
            return (source.parent / path_match.group(1)).resolve()
        if PATH_SETTING.search(attribute) is not None:
            raise RuntimeError(
                f"unsupported conditional or nonstandard path attribute in {source}: "
                f"{attribute!r}"
            )

    name_match = re.search(r"\bmod\s+([A-Za-z_][A-Za-z0-9_]*)", item)
    if name_match is None:
        raise RuntimeError(f"cannot resolve module declaration {item!r}")
    name = name_match.group(1)
    base = (
        source.parent
        if source.name in {"lib.rs", "main.rs", "mod.rs"}
        else source.parent / source.stem
    )
    direct = base / f"{name}.rs"
    nested = base / name / "mod.rs"
    if direct.exists():
        return direct.resolve()
    if nested.exists():
        return nested.resolve()
    raise RuntimeError(f"module {name!r} from {source} was not found")


def resolve_include_file(source: Path, item: str) -> Path | None:
    match = INCLUDE_INVOCATION.fullmatch(item)
    if match is None:
        if INCLUDE_PREFIX.match(item) and GENERATED_INCLUDE.fullmatch(item) is None:
            raise RuntimeError(f"unsupported nonliteral include in {source}")
        return None
    literal = match.group("literal")
    if literal.startswith("r"):
        first_quote = literal.find('"')
        hashes = literal[1:first_quote]
        suffix = '"' + hashes
        value = literal[first_quote + 1 : -len(suffix)]
    else:
        try:
            value = json.loads(literal)
        except json.JSONDecodeError as error:
            raise RuntimeError(f"unsupported include literal in {source}") from error
    return (source.parent / value).resolve(strict=True)


def repository_file(repo: Path, path: Path) -> Path:
    resolved = path.resolve(strict=True)
    try:
        resolved.relative_to(repo.resolve(strict=True))
    except ValueError as error:
        raise RuntimeError("Rust module reference escaped the repository") from error
    if not resolved.is_file():
        raise RuntimeError("Rust module reference target is not a file")
    return resolved


def reference_inventory(
    repo: Path,
    rule: Path,
    sources: Iterable[Path],
    ast_matches: AstMatches,
    attribute_matches: AttributeMatches,
    test_only_ranges: TestOnlyRanges,
) -> dict[FileIdentity, ReferenceRecord]:
    inventory: dict[FileIdentity, ReferenceRecord] = {}
    queue = [
        (repository_file(repo, source), "production") for source in sorted(sources)
    ]
    processed: set[tuple[FileIdentity, str]] = set()
    while queue:
        source, source_kind = queue.pop(0)
        source_key = (file_identity(source), source_kind)
        if source_key in processed:
            continue
        processed.add(source_key)
        test_ranges, _modules = test_only_ranges(repo, rule, source)
        attributes = attribute_matches(repo, source)
        path_attributes = {
            byte_range(attribute)
            for attribute in attributes
            if PATH_SETTING.search(attribute["text"]) is not None
        }
        associated_paths: set[ByteRange] = set()
        data = source.read_bytes()
        for item in ast_matches(repo, rule, source):
            text = item["text"].strip()
            module_declaration = MODULE_DECLARATION.fullmatch(text) is not None
            include_target = resolve_include_file(source, text)
            if not module_declaration and include_target is None:
                continue
            item_start, item_end = byte_range(item)
            associated = (
                associated_attributes(data, item_start, attributes)
                if module_declaration
                else []
            )
            if module_declaration:
                associated_paths.update(
                    byte_range(attribute)
                    for attribute in associated
                    if PATH_SETTING.search(attribute["text"]) is not None
                )
            target = repository_file(
                repo,
                resolve_module_file(
                    source,
                    (attribute["text"].strip() for attribute in associated),
                    text,
                )
                if module_declaration
                else include_target,
            )
            test_only = any(
                start <= item_start and item_end <= end for start, end in test_ranges
            )
            kind = "test" if source_kind == "test" or test_only else "production"
            record_reference(inventory, target, kind)
            relative = target.relative_to(repo.resolve(strict=True))
            if not relative.parts or relative.parts[0] != "crates":
                queue.append((target, kind))
        unassociated_paths = path_attributes - associated_paths
        if unassociated_paths:
            raise RuntimeError(
                f"{source}: path attributes were not associated with external modules: "
                f"{sorted(unassociated_paths)}"
            )
    return inventory


def is_crate_build_target(repo: Path, path: Path) -> bool:
    relative = path.resolve().relative_to(repo.resolve())
    return (
        len(relative.parts) == 3
        and relative.parts[0] == "crates"
        and relative.parts[2] == "build.rs"
    )


def test_only_identities(
    repo: Path, inventory: dict[FileIdentity, ReferenceRecord]
) -> set[FileIdentity]:
    return {
        identity
        for identity, record in inventory.items()
        if record.kinds == {"test"} and not is_crate_build_target(repo, record.path)
    }


def discover_reference_inventory(
    repo: Path,
    rule: Path,
    ast_matches: AstMatches,
    attribute_matches: AttributeMatches,
    test_only_ranges: TestOnlyRanges,
) -> dict[FileIdentity, ReferenceRecord]:
    return reference_inventory(
        repo,
        rule,
        repo.glob("crates/**/*.rs"),
        ast_matches,
        attribute_matches,
        test_only_ranges,
    )


def discover_test_only_files(
    repo: Path,
    rule: Path,
    ast_matches: AstMatches,
    attribute_matches: AttributeMatches,
    test_only_ranges: TestOnlyRanges,
) -> set[FileIdentity]:
    return test_only_identities(
        repo,
        discover_reference_inventory(
            repo, rule, ast_matches, attribute_matches, test_only_ranges
        ),
    )


def validate_fixtures(
    repo: Path,
    rule: Path,
    ast_matches: AstMatches,
    attribute_matches: AttributeMatches,
    test_only_ranges: TestOnlyRanges,
) -> dict[FileIdentity, ReferenceRecord]:
    fixtures = repo / "docs/reviews/evidence/fixtures/module_references"
    inventory = reference_inventory(
        repo,
        rule,
        [fixtures / "owner.rs"],
        ast_matches,
        attribute_matches,
        test_only_ranges,
    )
    relative = {
        str(record.path.relative_to(fixtures.resolve())): record.kinds
        for record in inventory.values()
    }
    expected = {
        "production_only/mod.rs": {"production"},
        "shared/mod.rs": {"production", "test"},
        "test_only/mod.rs": {"test"},
        "included/mod.rs": {"production"},
        "test_included/mod.rs": {"test"},
    }
    if relative != expected:
        raise RuntimeError(
            "module-reference fixtures produced unexpected provenance: "
            f"expected {expected}, found {relative}"
        )
    classified = {path for path, kinds in relative.items() if kinds == {"test"}}
    if classified != {"test_only/mod.rs", "test_included/mod.rs"}:
        raise RuntimeError(
            "module-reference fixtures misclassified production-reachable files: "
            f"{classified}"
        )
    synthetic: dict[FileIdentity, ReferenceRecord] = {}
    shared_identity: FileIdentity = ("inode", 1, 1)
    record_reference(synthetic, fixtures / "shared/mod.rs", "test", shared_identity)
    record_reference(
        synthetic, fixtures / "SHARED/mod.rs", "production", shared_identity
    )
    if len(synthetic) != 1 or next(iter(synthetic.values())).kinds != {
        "test",
        "production",
    }:
        raise RuntimeError("case aliases were not grouped by file identity")
    if shared_identity in test_only_identities(repo, synthetic):
        raise RuntimeError("case-aliased production reference was classified test-only")
    distinct: dict[FileIdentity, ReferenceRecord] = {}
    record_reference(distinct, fixtures / "shared/mod.rs", "test", ("inode", 1, 2))
    record_reference(
        distinct, fixtures / "SHARED/mod.rs", "production", ("inode", 1, 3)
    )
    if len(distinct) != 2:
        raise RuntimeError("distinct files were merged by path spelling")
    return inventory
