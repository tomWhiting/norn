"""Build the P2 fixture and evidence redaction inventory."""

from __future__ import annotations

import hashlib
import re
from pathlib import Path
from typing import Any


SOURCE_PRIVATE_PATH = re.compile(
    r"/(?:Users|home)/(?!(?:test|user|operator)(?:/|\b))[^/\s\"']+"
)
SOURCE_SYNTHETIC_EMAILS = frozenset(
    {
        "fixture@example.invalid",
        "private@example.com",
        "secret@chatgpt.com",
    }
)


def scan_source_fixture(path: str, data: bytes, scanner: Any) -> dict[str, Any]:
    """Reject high-confidence private material without rejecting redaction fixtures."""
    findings = []
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        text = ""
        findings.append({"rule": "artifact_integrity", "location": path})
    if (
        scanner.OPENAI_KEY.search(text)
        or scanner.JWT.search(text)
        or scanner.PRIVATE_KEY.search(text)
    ):
        findings.append({"rule": "credential_material", "location": path})
    bearer = scanner.BEARER.search(text)
    if bearer is not None and not scanner._safe_placeholder(bearer.group(1)):
        findings.append({"rule": "credential_material", "location": path})
    emails = {match.group(1).lower() for match in scanner.EMAIL.finditer(text)}
    private_emails = {
        email
        for email in emails
        if email not in SOURCE_SYNTHETIC_EMAILS
        and not email.rsplit("@", 1)[-1].endswith((".example", ".invalid", ".test"))
    }
    if private_emails:
        findings.append({"rule": "private_prompt_content", "location": path})
    if SOURCE_PRIVATE_PATH.search(text):
        findings.append({"rule": "absolute_private_path", "location": path})
    counts = {rule: 0 for rule in scanner.RULES}
    for finding in findings:
        counts[finding["rule"]] += 1
    return {
        "path": path,
        "category": "phase_fixture",
        "sha256": hashlib.sha256(data).hexdigest(),
        "bytes": len(data),
        "rule_matches": counts,
        "historical_absolute_path_disclosures": 0,
        "findings": findings,
        "passed": not findings,
    }


def build(
    repo: Path,
    base: str,
    source: str,
    historical: dict[str, str],
    generated: dict[str, bytes],
    scanner: Any,
    git: Any,
) -> dict[str, Any]:
    changed = git(repo, "diff", "--name-only", "-z", f"{base}...{source}")
    fixture_paths = []
    evidence_paths = set(historical)
    for raw in changed.split(b"\0"):
        if not raw:
            continue
        path = raw.decode()
        if path.startswith("docs/reviews/evidence/") and path.endswith(".json"):
            evidence_paths.add(path)
        if not path.startswith("crates/") or not path.endswith(".rs"):
            continue
        data = git(repo, "show", f"{source}:{path}")
        parts = Path(path).parts
        if any("test" in part.lower() or "fixture" in part.lower() for part in parts):
            fixture_paths.append(path)
        elif b"#[cfg(test)]" in data:
            fixture_paths.append(path)

    records = []
    for path in sorted(fixture_paths):
        records.append(
            scan_source_fixture(
                path,
                git(repo, "show", f"{source}:{path}"),
                scanner,
            )
        )
    for path in sorted(evidence_paths):
        data = git(repo, "show", f"{source}:{path}")
        record = scanner.scan_payload(path, data, "retained_historical", False)
        expected = historical.get(path)
        if expected is not None and scanner._sha256(data) != expected:
            record["findings"].append(
                {"rule": "artifact_integrity", "location": path}
            )
            record["rule_matches"]["artifact_integrity"] += 1
            record["passed"] = False
        records.append(record)
    for label, data in sorted(generated.items()):
        records.append(scanner.scan_payload(label, data, "generated_final", True))

    findings = [finding for record in records for finding in record["findings"]]
    return {
        "schema_version": 1,
        "kind": "p2_final_redaction",
        "base": base,
        "source": source,
        "rules": scanner.RULES,
        "method": {
            "phase_fixtures": "whole changed Rust blobs containing test or fixture code",
            "source_fixture_policy": (
                "reject key/JWT/private-key/bearer patterns, non-reserved email, and "
                "non-synthetic user-home paths; credential-shaped test values and "
                "account-ID shape require independent source inspection"
            ),
            "retained": "all P2-range evidence JSON plus hash-pinned historical evidence",
            "generated": "gate, policy, and distribution artifacts before attestation",
            "semantic_limit": "reviewer inspection remains required for arbitrary prose",
        },
        "inventory": records,
        "summary": {
            "files": len(records),
            "phase_fixtures": len(fixture_paths),
            "retained_historical": len(evidence_paths),
            "generated_final": len(generated),
            "findings": len(findings),
        },
        "findings": findings,
        "passed": not findings,
    }
