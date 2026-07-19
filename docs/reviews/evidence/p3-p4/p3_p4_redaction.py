"""Deterministic disclosure gate for final P3/P4 fixtures and evidence."""

from __future__ import annotations

import hashlib
import json
import re
import subprocess
from pathlib import Path
from typing import Any, Final, Iterable

import p0_evidence_disclosure as disclosure
from p0_evidence_io import strict_json_loads


RULES: Final = {
    "credential_material": "API keys, bearer credentials, JWTs, and private keys",
    "real_account_identifier": "non-fixture account identifiers",
    "private_prompt_content": "personal prompt content and private contact data",
    "reusable_turn_state": "reusable response, conversation, or encrypted turn state",
    "raw_cache_key": "non-placeholder cache identity",
    "absolute_private_path": "absolute user or private temporary paths",
    "artifact_integrity": "UTF-8 and JSON artifact integrity",
}
SAFE_PLACEHOLDER: Final = re.compile(
    r"^(?:dummy|fake|fixture|placeholder|redacted|sample|sentinel|synthetic|test)"
    r"(?:[-_:](?:account|api[-_]?key(?:[-_]?value)?|bearer|cache[-_]?key|"
    r"conversation[-_]?id|credential|id|key|response[-_]?id|secret|token|"
    r"turn[-_]?state|value))?$"
)
SAFE_TYPED_PLACEHOLDER: Final = re.compile(
    r"^(?:account|acct|cache|conversation|conv|response|resp|token|turn)[-_]"
    r"(?:dummy|fake|fixture|placeholder|redacted|sample|sentinel|synthetic|test)$"
)
SAFE_LITERAL_PLACEHOLDERS: Final = frozenset(
    {"dispatch-access-token", "rejected-dispatch-token"}
)
SAFE_CACHE_KEYS: Final = frozenset(
    {"ck", "explicit-key", "session-cache", "stable-cache-key"}
)
OPENAI_KEY: Final = re.compile(
    r"(?<![A-Za-z0-9_-])sk-(?:(?:proj|svcacct)-)?[A-Za-z0-9_-]{16,}"
)
JWT: Final = re.compile(
    r"(?<![A-Za-z0-9_-])eyJ[A-Za-z0-9_-]{8,}\."
    r"[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}(?![A-Za-z0-9_-])"
)
PRIVATE_KEY: Final = re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----")
BEARER: Final = re.compile(r"\bBearer\s+([A-Za-z0-9._~+/=-]{12,})", re.IGNORECASE)
SECRET_FIELD: Final = re.compile(
    r"(?i)(?:access[_-]?token|refresh[_-]?token|id[_-]?token|api[_-]?key)"
    r"[\"']?\s*(?::|=|=>)\s*(?:Some\()?\s*[\"']([^\"']{12,})"
)
ACCOUNT_FIELD: Final = re.compile(
    r"(?i)(?:chatgpt-account-id|account[_-]?id)"
    r"[\"']?\s*(?::|=|=>)\s*(?:Some\()?\s*[\"']([^\"']+)"
)
TURN_FIELD: Final = re.compile(
    r"(?i)(?:previous_response_id|conversation[_-]?id|turn[_-]?state|encrypted_content)"
    r"[\"']?\s*(?::|=|=>)\s*(?:Some\()?\s*[\"']([^\"']+)"
)
CACHE_FIELD: Final = re.compile(
    r"(?i)(?:prompt_cache_key|cache_key)"
    r"[\"']?\s*(?::|=|=>)\s*(?:Some\()?\s*[\"']([^\"']+)"
)
EMAIL: Final = re.compile(
    r"(?<![A-Za-z0-9._%+-])([A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,})"
)
PRIVATE_PROMPT_MARKER: Final = re.compile(
    r"(?i)(?:private[_ -]?prompt|my\s+(?:password|secret|api key)\s+is)"
)
USER_PATH: Final = re.compile(
    r"(?i)(?:/(?:Users|home)/[^/\s\"']+(?:/[^\s\"']*)?"
    r"|/root(?:/[^\s\"']*)?|/(?:tmp|private/(?:tmp|var)|var/folders)"
    r"(?:/[^\s\"']*)?|[A-Z]:[\\/]Users[\\/][^\s\"']+)"
)
SYNTHETIC_FIXTURE_PATHS: Final = frozenset(
    {
        "/home/user",
        "/root",
        "/root/activity-child",
        "/root/child",
        "/root/other",
        "/root/spawn/worker",
        "/tmp",
        "/tmp/a",
        "/tmp/auth",
        "/tmp/child-moved",
        "/tmp/debug",
        "/tmp/foo.rs",
        "/tmp/norn-cli-slash-actions",
        "/tmp/norn-cli-test",
        "/tmp/norn-debug",
        "/tmp/parent-wd",
        "/tmp/project",
        "/tmp/project/.agents/skills",
        "/tmp/project/.claude/skills",
        "/tmp/project/.norn/skills",
        "/tmp/sentinel-dump-path",
        "/tmp/some_file.txt",
        "/tmp/test",
        "/tmp/workspace-root",
        "/tmp/x",
    }
)
OPAQUE_ID: Final = re.compile(r"^[A-Za-z0-9_-]{20,}$")
UUID: Final = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-"
    r"[89ab][0-9a-f]{3}-[0-9a-f]{12}$",
    re.IGNORECASE,
)


def _git(repo: Path, *args: str) -> bytes:
    return subprocess.run(
        ["git", *args], cwd=repo, check=True, stdout=subprocess.PIPE
    ).stdout


def _sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _safe_placeholder(value: str) -> bool:
    lowered = value.strip().lower()
    if lowered in {"<redacted>", "[redacted]"} or lowered in SAFE_LITERAL_PLACEHOLDERS:
        return True
    return (
        SAFE_PLACEHOLDER.fullmatch(lowered) is not None
        or SAFE_TYPED_PLACEHOLDER.fullmatch(lowered) is not None
    )


def _real_identifier(value: str) -> bool:
    return not _safe_placeholder(value) and (
        UUID.fullmatch(value) is not None
        or OPAQUE_ID.fullmatch(value) is not None
        or re.fullmatch(r"acct[_-][A-Za-z0-9_-]{12,}", value) is not None
    )


def _safe_cache_key(value: str) -> bool:
    stripped = value.strip()
    return not stripped or _safe_placeholder(stripped) or stripped in SAFE_CACHE_KEYS


def _private_email(value: str) -> bool:
    for match in EMAIL.finditer(value):
        domain = match.group(1).rsplit("@", 1)[-1].lower()
        if not domain.endswith((".example", ".invalid", ".test")):
            return True
    return False


def _has_absolute_path(value: str) -> bool:
    logical = value.replace("<repo>/", "repo/").replace("<repo>\\", "repo\\")
    return disclosure.string_has_absolute_path(logical)


def _private_fixture_path(value: str) -> bool:
    return any(
        match.group(0).rstrip("/") not in SYNTHETIC_FIXTURE_PATHS
        for match in USER_PATH.finditer(value)
    )


def _finding(rule: str, location: str) -> dict[str, str]:
    return {"rule": rule, "location": location}


def _generic_findings(value: str, location: str) -> list[dict[str, str]]:
    findings = []
    if OPENAI_KEY.search(value) or JWT.search(value) or PRIVATE_KEY.search(value):
        findings.append(_finding("credential_material", location))
    bearer = BEARER.search(value)
    if bearer is not None and not _safe_placeholder(bearer.group(1)):
        findings.append(_finding("credential_material", location))
    for match in SECRET_FIELD.finditer(value):
        if not _safe_placeholder(match.group(1)):
            findings.append(_finding("credential_material", location))
            break
    for regex, rule in (
        (ACCOUNT_FIELD, "real_account_identifier"),
        (TURN_FIELD, "reusable_turn_state"),
    ):
        if any(_real_identifier(match.group(1)) for match in regex.finditer(value)):
            findings.append(_finding(rule, location))
    if any(not _safe_cache_key(match.group(1)) for match in CACHE_FIELD.finditer(value)):
        findings.append(_finding("raw_cache_key", location))
    is_rule_identifier = value.strip().lower() in RULES
    if (
        not is_rule_identifier and PRIVATE_PROMPT_MARKER.search(value)
    ) or _private_email(value):
        findings.append(_finding("private_prompt_content", location))
    return findings


def _json_strings(
    value: Any, pointer: str = "", context_key: str = ""
) -> Iterable[tuple[str, str, str]]:
    if isinstance(value, dict):
        for index, key in enumerate(sorted(value)):
            key_pointer = f"{pointer}/~key:{index}"
            yield key_pointer, "", key
            yield from _json_strings(value[key], key_pointer, key)
    elif isinstance(value, list):
        for index, child in enumerate(value):
            yield from _json_strings(child, f"{pointer}/{index}", context_key)
    elif isinstance(value, str):
        yield pointer or "/", context_key, value


def _context_findings(key: str, value: str, location: str) -> list[dict[str, str]]:
    normalized = key.lower().replace("-", "_")
    findings = []
    if normalized in {"access_token", "refresh_token", "id_token", "api_key"}:
        if len(value) >= 12 and not _safe_placeholder(value):
            findings.append(_finding("credential_material", location))
    if normalized in {"account_id", "chatgpt_account_id"} and _real_identifier(value):
        findings.append(_finding("real_account_identifier", location))
    if normalized in {
        "previous_response_id",
        "conversation_id",
        "turn_state",
        "encrypted_content",
    } and _real_identifier(value):
        findings.append(_finding("reusable_turn_state", location))
    if normalized in {"cache_key", "prompt_cache_key"} and not _safe_cache_key(value):
        findings.append(_finding("raw_cache_key", location))
    if normalized in {"prompt", "instructions", "input", "content", "text"} and (
        PRIVATE_PROMPT_MARKER.search(value) or _private_email(value)
    ):
        findings.append(_finding("private_prompt_content", location))
    return findings


def scan_payload(
    label: str, data: bytes, category: str, enforce_absolute_paths: bool
) -> dict[str, Any]:
    findings: list[dict[str, str]] = []
    path_disclosures = 0
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        findings.append(_finding("artifact_integrity", label))
        text = ""
    if category == "phase_fixture":
        findings.extend(_generic_findings(text, label))
        if _private_fixture_path(text):
            findings.append(_finding("absolute_private_path", label))
    else:
        try:
            document = strict_json_loads(data)
        except (json.JSONDecodeError, ValueError):
            findings.append(_finding("artifact_integrity", label))
            document = None
        if document is not None:
            for pointer, key, value in _json_strings(document):
                location = f"{label}#{pointer}"
                findings.extend(_generic_findings(value, location))
                findings.extend(_context_findings(key, value, location))
                if _has_absolute_path(value):
                    if enforce_absolute_paths:
                        findings.append(_finding("absolute_private_path", location))
                    else:
                        path_disclosures += 1
    findings = sorted(
        {json.dumps(item, sort_keys=True): item for item in findings}.values(),
        key=lambda item: (item["rule"], item["location"]),
    )
    counts = {rule: 0 for rule in RULES}
    for finding in findings:
        counts[finding["rule"]] += 1
    return {
        "path": label,
        "category": category,
        "sha256": _sha256(data),
        "bytes": len(data),
        "rule_matches": counts,
        "historical_absolute_path_disclosures": path_disclosures,
        "findings": findings,
        "passed": not findings,
    }


def _phase_fixture_paths(repo: Path, base: str, source: str) -> list[str]:
    changed = _git(repo, "diff", "--name-only", "-z", f"{base}...{source}")
    result = []
    for item in changed.split(b"\0"):
        if not item:
            continue
        path = item.decode()
        if not path.startswith("crates/") or not path.endswith(".rs"):
            continue
        data = _git(repo, "show", f"{source}:{path}")
        parts = Path(path).parts
        name = Path(path).name.lower()
        if any("test" in part.lower() or "fixture" in part.lower() for part in parts):
            result.append(path)
        elif b"#[cfg(test)]" in data or "test" in name or "fixture" in name:
            result.append(path)
    return sorted(result)


def _retained_paths(
    repo: Path, base: str, source: str, reused_artifacts: dict[str, str]
) -> list[str]:
    paths = set(reused_artifacts)
    raw = _git(repo, "diff", "--name-only", "-z", f"{base}...{source}")
    paths.update(
        item.decode()
        for item in raw.split(b"\0")
        if item
        and item.decode().startswith("docs/reviews/evidence/")
        and item.decode().endswith(".json")
    )
    return sorted(paths)


def build_redaction_evidence(
    repo: Path,
    base: str,
    source: str,
    reused_artifacts: dict[str, str],
    generated_artifacts: dict[str, bytes],
) -> dict[str, Any]:
    tree = _git(repo, "rev-parse", f"{source}^{{tree}}").decode().strip()
    records = []
    for path in _phase_fixture_paths(repo, base, source):
        records.append(
            scan_payload(path, _git(repo, "show", f"{source}:{path}"), "phase_fixture", True)
        )
    for path in _retained_paths(repo, base, source, reused_artifacts):
        data = _git(repo, "show", f"{source}:{path}")
        expected = reused_artifacts.get(path)
        if expected is not None and _sha256(data) != expected:
            record = scan_payload(path, data, "retained_historical", False)
            record["findings"].append(_finding("artifact_integrity", path))
            record["rule_matches"]["artifact_integrity"] += 1
            record["passed"] = False
        else:
            record = scan_payload(path, data, "retained_historical", False)
        records.append(record)
    for label, data in sorted(generated_artifacts.items()):
        records.append(scan_payload(label, data, "generated_final", True))
    findings = [finding for record in records for finding in record["findings"]]
    return {
        "schema_version": 1,
        "kind": "p3_p4_final_redaction",
        "base": base,
        "source": source,
        "tree": tree,
        "rules": RULES,
        "method": {
            "phase_fixture_inventory": (
                "whole source blobs for changed crates/**/*.rs paths whose path "
                "or source identifies test or fixture code"
            ),
            "retained_inventory": (
                "all changed JSON below docs/reviews/evidence in the phase range "
                "plus every hash-pinned reused artifact"
            ),
            "generated_inventory": "final gate, policy, and distributions before attestation",
            "placeholder_policy": (
                "exact redaction tokens or structured leading fixture markers; "
                "arbitrary substring matches are rejected"
            ),
            "synthetic_path_policy": (
                "only exact checked-in fixture paths in SYNTHETIC_FIXTURE_PATHS; "
                "unlisted roots and descendants are rejected"
            ),
            "semantic_limit": (
                "arbitrary human prose cannot be classified as private mechanically; "
                "the hash-bound inventory supports mandatory reviewer inspection"
            ),
        },
        "inventory": records,
        "summary": {
            "files": len(records),
            "phase_fixtures": sum(r["category"] == "phase_fixture" for r in records),
            "retained_historical": sum(
                r["category"] == "retained_historical" for r in records
            ),
            "generated_final": sum(r["category"] == "generated_final" for r in records),
            "historical_absolute_path_disclosures": sum(
                r["historical_absolute_path_disclosures"] for r in records
            ),
            "findings": len(findings),
        },
        "findings": findings,
        "passed": not findings,
    }


def redaction_document_valid(actual: object, expected: dict[str, Any]) -> bool:
    return actual == expected and expected.get("passed") is True
