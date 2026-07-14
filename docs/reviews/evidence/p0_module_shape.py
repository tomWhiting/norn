"""Repository-wide `mod.rs` shape policy and its checked-in fixtures."""

import re
from pathlib import Path
from typing import Callable

import p0_module_references as module_references


ByteRange = tuple[int, int]
AstMatches = Callable[[Path, Path, Path], list[dict]]
TestOnlyRanges = Callable[[Path, Path, Path], tuple[list[ByteRange], list[Path]]]

MODULE_DECLARATION = re.compile(
    r"(?:pub(?:\([^)]*\))?\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*;\Z",
    re.DOTALL,
)
MODULE_REEXPORT = re.compile(
    r"pub(?:\([^)]*\))?\s+use\s+.+;\Z",
    re.DOTALL,
)
RULE_KIND_FIXTURES = {
    "const_item": "const PRODUCTION_CONST",
    "enum_item": "enum ProductionEnum",
    "enum_variant": "ProductionVariant",
    "extern_crate_declaration": "extern crate core",
    "field_declaration": "production_field: usize",
    "foreign_mod_item": 'extern "C"',
    "function_item": "fn production_logic",
    "function_signature_item": "fn required_method();",
    "impl_item": "impl ProductionType",
    "let_declaration": "let production_local",
    "macro_definition": "macro_rules! production_macro",
    "macro_invocation": "production_macro!",
    "mod_item": "mod inline_logic",
    "static_item": "static PRODUCTION_STATIC",
    "struct_item": "struct ProductionType",
    "trait_item": "trait ProductionTrait",
    "type_item": "type ProductionAlias",
    "union_item": "union ProductionUnion",
    "use_declaration": "use private_alias",
}


def byte_range(match: dict) -> ByteRange:
    offsets = match["range"]["byteOffset"]
    return offsets["start"], offsets["end"]


def violations_for_source(
    repo: Path,
    rule: Path,
    source: Path,
    ast_matches: AstMatches,
    test_only_ranges: TestOnlyRanges,
) -> list[dict]:
    test_ranges, _modules = test_only_ranges(repo, rule, source)
    forbidden: list[tuple[ByteRange, dict]] = []
    for item in ast_matches(repo, rule, source):
        item_range = byte_range(item)
        if any(
            start <= item_range[0] and item_range[1] <= end
            for start, end in test_ranges
        ):
            continue
        text = item["text"].strip()
        if MODULE_DECLARATION.fullmatch(text) or MODULE_REEXPORT.fullmatch(text):
            continue
        forbidden.append((item_range, item))

    # One outer production item is sufficient evidence; nested fields and
    # functions would otherwise duplicate the same structural violation.
    outermost = [
        item
        for item_range, item in forbidden
        if not any(
            other_range[0] <= item_range[0]
            and item_range[1] <= other_range[1]
            and item_range != other_range
            for other_range, _other in forbidden
        )
    ]
    relative = source.relative_to(repo)
    return [
        {
            "file": str(relative),
            "line": item["range"]["start"]["line"] + 1,
            "text": item["text"].splitlines()[0].strip(),
        }
        for item in outermost
    ]


def repository_violations(
    repo: Path,
    rule: Path,
    test_only_files: set[module_references.FileIdentity],
    references: dict[module_references.FileIdentity, module_references.ReferenceRecord],
    ast_matches: AstMatches,
    test_only_ranges: TestOnlyRanges,
) -> list[dict]:
    violations: list[dict] = []
    candidates = {
        module_references.file_identity(source): source.resolve(strict=True)
        for source in repo.glob("crates/**/mod.rs")
    }
    for identity, record in references.items():
        if "production" in record.kinds and record.path.name.casefold() == "mod.rs":
            candidates.setdefault(identity, record.path)
    for identity, source in sorted(candidates.items(), key=lambda item: str(item[1])):
        if identity in test_only_files:
            continue
        violations.extend(
            violations_for_source(repo, rule, source, ast_matches, test_only_ranges)
        )
    return violations


def validate_reference_fixtures(
    repo: Path,
    rule: Path,
    references: dict[module_references.FileIdentity, module_references.ReferenceRecord],
    ast_matches: AstMatches,
    test_only_ranges: TestOnlyRanges,
) -> None:
    test_only = module_references.test_only_identities(repo, references)
    violations = []
    for identity, record in references.items():
        if record.path.name.casefold() != "mod.rs" or identity in test_only:
            continue
        violations.extend(
            violations_for_source(
                repo, rule, record.path, ast_matches, test_only_ranges
            )
        )
    rejected = {record["file"] for record in violations}
    expected_suffixes = {
        "production_only/mod.rs",
        "shared/mod.rs",
        "included/mod.rs",
    }
    if {
        suffix
        for suffix in expected_suffixes
        if any(path.endswith(suffix) for path in rejected)
    } != expected_suffixes:
        raise RuntimeError(
            "production-reachable external mod.rs fixtures escaped shape policy"
        )
    if any(
        path.endswith(("test_only/mod.rs", "test_included/mod.rs")) for path in rejected
    ):
        raise RuntimeError("test-only external mod.rs fixture was shape-checked")


def validate_fixtures(
    repo: Path,
    rule: Path,
    ast_matches: AstMatches,
    test_only_ranges: TestOnlyRanges,
) -> None:
    fixtures = repo / "docs/reviews/evidence/fixtures/mod_shape"
    allowed = violations_for_source(
        repo, rule, fixtures / "allowed.rs", ast_matches, test_only_ranges
    )
    rejected = violations_for_source(
        repo, rule, fixtures / "rejected.rs", ast_matches, test_only_ranges
    )
    rejected_text = "\n".join(item["text"] for item in rejected)
    raw_rejected_text = "\n".join(
        item["text"] for item in ast_matches(repo, rule, fixtures / "rejected.rs")
    )
    if allowed:
        raise RuntimeError(f"allowed mod.rs policy fixture was rejected: {allowed}")
    for expected in (
        "use private_alias",
        "const PRODUCTION_CONST",
        "static PRODUCTION_STATIC",
        "fn production_logic",
        "struct ProductionType",
        "enum ProductionEnum",
        "union ProductionUnion",
        "trait ProductionTrait",
        "impl ProductionType",
        "type ProductionAlias",
        "extern crate core",
        'extern "C"',
        "macro_rules! production_macro",
        "production_macro!",
        "mod inline_logic",
    ):
        if expected not in rejected_text:
            raise RuntimeError(
                f"mod.rs policy fixture did not reject {expected!r}: {rejected}"
            )
    configured_kinds = set(
        re.findall(r"^\s*-\s+kind:\s+([A-Za-z0-9_]+)\s*$", rule.read_text(), re.M)
    )
    if configured_kinds != RULE_KIND_FIXTURES.keys():
        raise RuntimeError(
            "Rust-item rule and fixture category inventory differ: "
            f"rule={sorted(configured_kinds)}, "
            f"fixtures={sorted(RULE_KIND_FIXTURES)}"
        )
    for kind, expected in RULE_KIND_FIXTURES.items():
        if expected not in raw_rejected_text:
            raise RuntimeError(
                f"Rust-item rule fixture did not match {kind} via {expected!r}"
            )
