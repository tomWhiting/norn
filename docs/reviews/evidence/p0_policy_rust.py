"""Rust AST and cfg(test) helpers for the P0 policy evidence runner."""

from __future__ import annotations

import itertools
import json
import re
import subprocess
from dataclasses import dataclass
from pathlib import Path

import p0_module_references as module_references


CFG_PATTERN = "#[cfg($$$COND)]"
ATTRIBUTE_PATTERN = "#[$$$ATTR]"


def _json_lines(raw: str) -> list[dict]:
    return [json.loads(line) for line in raw.splitlines() if line.strip()]


def _ast_command(*args: str, cwd: Path) -> str:
    result = subprocess.run(
        args,
        cwd=cwd,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode not in {0, 1}:
        raise RuntimeError(
            result.stderr.strip() or f"ast-grep exited {result.returncode}"
        )
    return result.stdout


@dataclass(frozen=True)
class Token:
    kind: str
    text: str


def cfg_tokens(expression: str) -> list[Token]:
    pattern = re.compile(
        r"\s*(?:(?P<ident>[A-Za-z_][A-Za-z0-9_-]*)|"
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


def _cfg_expression(attribute: str) -> str:
    prefix = "#[cfg("
    if not attribute.startswith(prefix) or not attribute.endswith(")]"):
        raise ValueError(f"not a cfg attribute: {attribute!r}")
    return attribute[len(prefix) : -2]


def _requires_test(attribute: str) -> bool:
    return True not in CfgParser(_cfg_expression(attribute)).parse()


def ast_matches(repo: Path, rule: Path, source: Path) -> list[dict]:
    raw = _ast_command(
        "ast-grep",
        "scan",
        "--rule",
        str(rule),
        "--json=stream",
        str(source),
        cwd=repo,
    )
    return _json_lines(raw)


def _cfg_matches(repo: Path, source: Path) -> list[dict]:
    raw = _ast_command(
        "ast-grep",
        "run",
        "-p",
        CFG_PATTERN,
        "--json=stream",
        str(source),
        cwd=repo,
    )
    return _json_lines(raw)


def attribute_matches(repo: Path, source: Path) -> list[dict]:
    raw = _ast_command(
        "ast-grep",
        "run",
        "-p",
        ATTRIBUTE_PATTERN,
        "--json=stream",
        str(source),
        cwd=repo,
    )
    return _json_lines(raw)


def _byte_range(match: dict) -> tuple[int, int]:
    offsets = match["range"]["byteOffset"]
    return offsets["start"], offsets["end"]


def test_only_ranges(
    repo: Path, rule: Path, source: Path
) -> tuple[list[tuple[int, int]], list[Path]]:
    data = source.read_bytes()
    items = sorted(
        (_byte_range(item), item["text"]) for item in ast_matches(repo, rule, source)
    )
    ranges: list[tuple[int, int]] = []
    modules: list[Path] = []
    for attribute in _cfg_matches(repo, source):
        if not _requires_test(attribute["text"]):
            continue
        attr_start, attr_end = _byte_range(attribute)
        item = next((entry for entry in items if entry[0][0] >= attr_end), None)
        if item is None:
            # The attribute can decorate a trailing statement, field, or
            # parameter. Retain it rather than understating production LOC.
            continue
        (item_start, item_end), item_text = item
        gap = data[attr_end:item_start].decode("utf-8")
        residue = re.sub(r"#\[[^\]]*\]", "", gap, flags=re.DOTALL).strip()
        if residue:
            # A cfg may decorate a statement, field, or parameter that is not in
            # the item inventory. Retaining it makes the production count a
            # conservative upper bound instead of misclassifying a later item.
            continue
        ranges.append((attr_start, item_end))
        if re.fullmatch(
            r"(?:pub(?:\([^)]*\))?\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*;", item_text
        ):
            attributes = data[attr_start:item_start].decode("utf-8")
            modules.append(
                module_references.resolve_module_file(
                    source,
                    re.findall(r"#\[[^\]]*\]", attributes, flags=re.DOTALL),
                    item_text,
                )
            )
    return merge_ranges(ranges), modules


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
