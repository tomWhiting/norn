"""AST-backed Rust literal masking for added-line policy checks."""

import json
import subprocess
from pathlib import Path
from typing import Final


LITERAL_RULE: Final = """\
id: rust-literal
language: Rust
rule:
  any:
    - kind: string_literal
    - kind: raw_string_literal
"""


def policy_lines(repo: Path, relative: str) -> list[str] | None:
    return (
        masked_literal_lines(repo, repo / relative)
        if relative.endswith(".rs")
        else None
    )


def masked_literal_lines(repo: Path, source: Path) -> list[str]:
    result = subprocess.run(
        (
            "ast-grep",
            "scan",
            "--inline-rules",
            LITERAL_RULE,
            "--json=stream",
            str(source),
        ),
        cwd=repo,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode not in {0, 1}:
        raise RuntimeError(
            result.stderr.strip() or f"ast-grep exited {result.returncode}"
        )
    masked = bytearray(source.read_bytes())
    for line in result.stdout.splitlines():
        if not line.strip():
            continue
        offsets = json.loads(line)["range"]["byteOffset"]
        for index in range(offsets["start"], offsets["end"]):
            if masked[index] not in {10, 13}:
                masked[index] = 32
    return masked.decode("utf-8").splitlines()
