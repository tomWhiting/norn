#!/usr/bin/env python3
"""Generate syntax-aware P0 production-LOC and added-line policy evidence."""

from __future__ import annotations

import argparse
import itertools
import json
import re
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path


CFG_PATTERN = "#[cfg($$$COND)]"
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
    "marker": re.compile(r"\b(?:TODO|FIXME|HACK)\b"),
}
ARTIFACT_WRITER = re.compile(
    r"(?:PrivateRoot::(?:create|open)|"
    r"\.(?:create_new|open_append_create|open_lock|rename|publish_new|"
    r"create_dir_all|remove_file|remove_dir_all|set_len)\s*\(|"
    r"(?:std::|tokio::)?fs::(?:write|rename|remove_file|remove_dir_all|"
    r"create_dir|create_dir_all|copy|hard_link|symlink)\s*\(|"
    r"(?:(?:std|tokio)::fs::)?File::create\s*\(|"
    r"(?:(?:std|tokio)::fs::)?OpenOptions::new\s*\()"
)


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


def json_lines(raw: str) -> list[dict]:
    return [json.loads(line) for line in raw.splitlines() if line.strip()]


def ast_command(*args: str, cwd: Path) -> str:
    result = subprocess.run(
        args,
        cwd=cwd,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode not in {0, 1}:
        raise RuntimeError(result.stderr.strip() or f"ast-grep exited {result.returncode}")
    return result.stdout


@dataclass(frozen=True)
class Token:
    kind: str
    text: str


def cfg_tokens(expression: str) -> list[Token]:
    pattern = re.compile(
        r'\s*(?:(?P<ident>[A-Za-z_][A-Za-z0-9_-]*)|'
        r'(?P<string>"(?:\\.|[^"\\])*")|(?P<punct>[(),=]))'
    )
    tokens: list[Token] = []
    offset = 0
    while offset < len(expression):
        match = pattern.match(expression, offset)
        if match is None:
            raise ValueError(f"unsupported cfg syntax near {expression[offset:]!r}")
        offset = match.end()
        kind = match.lastgroup
        if kind is None:
            raise ValueError("cfg tokenizer produced no token")
        tokens.append(Token(kind, match.group(kind)))
    return tokens


class CfgParser:
    def __init__(self, expression: str) -> None:
        self.tokens = cfg_tokens(expression)
        self.offset = 0

    def parse(self) -> set[bool]:
        values = self.parse_expression()
        if self.offset != len(self.tokens):
            raise ValueError("trailing cfg tokens")
        return values

    def take(self, text: str | None = None) -> Token:
        if self.offset >= len(self.tokens):
            raise ValueError("unexpected end of cfg expression")
        token = self.tokens[self.offset]
        if text is not None and token.text != text:
            raise ValueError(f"expected {text!r}, found {token.text!r}")
        self.offset += 1
        return token

    def parse_expression(self) -> set[bool]:
        name = self.take().text
        if self.offset < len(self.tokens) and self.tokens[self.offset].text == "=":
            self.take("=")
            self.take()
            return {False, True}
        if self.offset < len(self.tokens) and self.tokens[self.offset].text == "(":
            self.take("(")
            arguments: list[set[bool]] = []
            if self.tokens[self.offset].text != ")":
                while True:
                    arguments.append(self.parse_expression())
                    if self.tokens[self.offset].text != ",":
                        break
                    self.take(",")
            self.take(")")
            return combine_cfg(name, arguments)
        return {False} if name == "test" else {False, True}


def combine_cfg(name: str, arguments: list[set[bool]]) -> set[bool]:
    combinations = itertools.product(*arguments) if arguments else [()]
    if name == "all":
        return {all(values) for values in combinations}
    if name == "any":
        return {any(values) for values in combinations}
    if name == "not" and len(arguments) == 1:
        return {not value for value in arguments[0]}
    return {False, True}


def cfg_expression(attribute: str) -> str:
    prefix = "#[cfg("
    if not attribute.startswith(prefix) or not attribute.endswith(")]" ):
        raise ValueError(f"not a cfg attribute: {attribute!r}")
    return attribute[len(prefix) : -2]


def requires_test(attribute: str) -> bool:
    return True not in CfgParser(cfg_expression(attribute)).parse()


def ast_matches(repo: Path, rule: Path, source: Path) -> list[dict]:
    raw = ast_command(
        "ast-grep",
        "scan",
        "--rule",
        str(rule),
        "--json=stream",
        str(source),
        cwd=repo,
    )
    return json_lines(raw)


def cfg_matches(repo: Path, source: Path) -> list[dict]:
    raw = ast_command(
        "ast-grep",
        "run",
        "-p",
        CFG_PATTERN,
        "--json=stream",
        str(source),
        cwd=repo,
    )
    return json_lines(raw)


def byte_range(match: dict) -> tuple[int, int]:
    offsets = match["range"]["byteOffset"]
    return offsets["start"], offsets["end"]


def test_only_ranges(repo: Path, rule: Path, source: Path) -> tuple[list[tuple[int, int]], list[Path]]:
    data = source.read_bytes()
    items = sorted((byte_range(item), item["text"]) for item in ast_matches(repo, rule, source))
    ranges: list[tuple[int, int]] = []
    modules: list[Path] = []
    for attribute in cfg_matches(repo, source):
        if not requires_test(attribute["text"]):
            continue
        attr_start, attr_end = byte_range(attribute)
        item = next((entry for entry in items if entry[0][0] >= attr_end), None)
        if item is None:
            raise RuntimeError(f"{source}: cfg(test) has no following Rust item")
        (item_start, item_end), item_text = item
        gap = data[attr_end:item_start].decode("utf-8")
        residue = re.sub(r"#\[[^\]]*\]", "", gap, flags=re.DOTALL).strip()
        if residue:
            raise RuntimeError(
                f"{source}: non-attribute text between cfg(test) and item: {residue!r}"
            )
        ranges.append((attr_start, item_end))
        if re.fullmatch(r"(?:pub(?:\([^)]*\))?\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*;", item_text):
            attributes = data[attr_start:item_start].decode("utf-8")
            modules.append(resolve_module_file(source, attributes, item_text))
    return merge_ranges(ranges), modules


def resolve_module_file(source: Path, attributes: str, item: str) -> Path:
    path_match = re.search(r'#\[path\s*=\s*"([^"]+)"\]', attributes)
    if path_match is not None:
        return (source.parent / path_match.group(1)).resolve()
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
    raise RuntimeError(f"test-only module {name!r} from {source} was not found")


def merge_ranges(ranges: list[tuple[int, int]]) -> list[tuple[int, int]]:
    merged: list[tuple[int, int]] = []
    for start, end in sorted(ranges):
        if merged and start <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(end, merged[-1][1]))
        else:
            merged.append((start, end))
    return merged


def strip_ranges(source: str, ranges: list[tuple[int, int]]) -> str:
    data = source.encode("utf-8")
    output: list[bytes] = []
    cursor = 0
    for start, end in ranges:
        output.append(data[cursor:start])
        cursor = end
    output.append(data[cursor:])
    return b"".join(output).decode("utf-8")


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


def discover_test_only_files(repo: Path, rule: Path) -> set[Path]:
    discovered: set[Path] = set()
    for source in sorted(repo.glob("crates/**/*.rs")):
        _ranges, modules = test_only_ranges(repo, rule, source)
        discovered.update(modules)
    return {
        path
        for path in discovered
        if not is_crate_build_target(repo, path)
    }


def is_crate_build_target(repo: Path, path: Path) -> bool:
    relative = path.resolve().relative_to(repo.resolve())
    return (
        len(relative.parts) == 3
        and relative.parts[0] == "crates"
        and relative.parts[2] == "build.rs"
    )


def tokei_counts(root: Path, repo: Path) -> dict[str, int]:
    raw = command("tokei", "--output", "json", str(root), cwd=repo)
    payload = json.loads(raw)
    reports = payload.get("Rust", {}).get("reports", [])
    return {
        str(Path(report["name"]).resolve().relative_to(root.resolve())): report["stats"]["code"]
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
    new_line = 0
    for line in raw.splitlines():
        if line.startswith("+++ b/"):
            current_file = line[6:]
            continue
        hunk = re.match(r"@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@", line)
        if hunk:
            new_line = int(hunk.group(1))
            continue
        if line.startswith("+") and not line.startswith("+++"):
            content = line[1:]
            for name, pattern in FORBIDDEN.items():
                if pattern.search(content):
                    matches[name].append(
                        {"file": current_file, "line": new_line, "text": content.strip()}
                    )
            new_line += 1
        elif line.startswith(" "):
            new_line += 1
    return matches


def artifact_writer_candidates(
    repo: Path,
    rule: Path,
    test_only_files: set[Path],
) -> list[dict]:
    candidates: list[dict] = []
    for source in sorted(repo.glob("crates/**/*.rs")):
        relative = source.relative_to(repo)
        path_parts = relative.parts
        integration_test = (
            len(path_parts) >= 3
            and path_parts[0] == "crates"
            and "tests" in path_parts[2:]
        )
        if source.resolve() in test_only_files or integration_test:
            continue
        ranges, _modules = test_only_ranges(repo, rule, source)
        data = source.read_bytes()
        offset = 0
        for line_number, raw_line in enumerate(data.splitlines(keepends=True), start=1):
            in_test_item = any(start <= offset < end for start, end in ranges)
            line = raw_line.decode("utf-8")
            if not in_test_item and ARTIFACT_WRITER.search(line):
                candidates.append(
                    {
                        "file": str(relative),
                        "line": line_number,
                        "text": line.strip(),
                    }
                )
            offset += len(raw_line)
    return candidates


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="41ea210")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--output")
    args = parser.parse_args()

    repo = Path(__file__).resolve().parents[3]
    rule = Path(__file__).with_name("p0-rust-items.yml")
    files = changed_rust_files(repo, args.base, args.head)
    test_only_files = discover_test_only_files(repo, rule)

    records: list[dict] = []
    with tempfile.TemporaryDirectory(prefix="norn-p0-policy-") as directory:
        stripped_root = Path(directory)
        for source in files:
            relative = source.relative_to(repo)
            destination = stripped_root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            path_parts = relative.parts
            integration_test = len(path_parts) >= 3 and path_parts[0] == "crates" and "tests" in path_parts[2:]
            if source.resolve() in test_only_files or integration_test:
                ranges = [(0, len(source.read_bytes()))]
            else:
                ranges, _modules = test_only_ranges(repo, rule, source)
            stripped = strip_ranges(source.read_text(encoding="utf-8"), ranges)
            destination.write_text(stripped, encoding="utf-8")
            records.append(
                {
                    "file": str(relative),
                    "physical_lines": len(source.read_text(encoding="utf-8").splitlines()),
                    "removed_test_ranges": [{"start": start, "end": end} for start, end in ranges],
                }
            )
        counts = tokei_counts(stripped_root, repo)

    for record in records:
        record["production_code_lines"] = counts.get(record["file"], 0)
    records.sort(key=lambda record: (-record["production_code_lines"], record["file"]))
    over_limit = [record["file"] for record in records if record["production_code_lines"] > 500]
    thin_entrypoint_violations = [
        record["file"]
        for record in records
        if Path(record["file"]).name in {"lib.rs", "main.rs"}
        and record["production_code_lines"] > 200
    ]
    evidence = {
        "base": args.base,
        "head": command("git", "rev-parse", args.head, cwd=repo).strip(),
        "method": {
            "parser": command("ast-grep", "--version", cwd=repo).strip(),
            "counter": command("tokei", "--version", cwd=repo).strip(),
            "rule": str(rule.relative_to(repo)),
            "cfg_policy": "remove AST items whose cfg cannot be true with test=false",
            "artifact_candidates": (
                "repository-wide production/build-script lexical sweep for filesystem "
                "roots, opens, creates, mutations, publication, and removal; the retained "
                "candidate list is intentionally conservative and requires the adjacent "
                "manual ownership/lifetime classification"
            ),
        },
        "changed_rust_file_count": len(records),
        "test_only_file_count": sum(record["production_code_lines"] == 0 for record in records),
        "over_500": over_limit,
        "thin_entrypoint_violations": thin_entrypoint_violations,
        "files": records,
        "added_line_policy_matches": added_line_evidence(repo, args.base, args.head),
        "artifact_writer_candidates": artifact_writer_candidates(
            repo,
            rule,
            test_only_files,
        ),
    }
    rendered = json.dumps(evidence, indent=2, sort_keys=True) + "\n"
    if args.output:
        output = repo / args.output
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(rendered, encoding="utf-8")
    else:
        print(rendered, end="")


if __name__ == "__main__":
    main()
