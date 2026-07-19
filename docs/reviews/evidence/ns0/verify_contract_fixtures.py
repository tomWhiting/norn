#!/usr/bin/env python3
"""Verify the logical NS0 fixture cases without claiming canonical bytes."""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path
from typing import Any


class DuplicateKeyError(ValueError):
    """Raised when a JSON object repeats a key."""


def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise DuplicateKeyError(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def load_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=unique_object)


def file_sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def expect(condition: bool, message: str, errors: list[str]) -> None:
    if not condition:
        errors.append(message)


def validate_ref(value: Any, location: str, errors: list[str]) -> None:
    if not isinstance(value, dict):
        errors.append(f"{location}: reference must be an object")
        return
    expect(
        set(value) == {"domain", "kind", "native_value", "scope"},
        f"{location}: reference fields differ from DomainRefV1",
        errors,
    )
    expect(
        isinstance(value.get("domain"), str) and bool(value.get("domain")),
        f"{location}: domain must be a non-empty string",
        errors,
    )
    expect(
        isinstance(value.get("kind"), str) and bool(value.get("kind")),
        f"{location}: kind must be a non-empty string",
        errors,
    )
    scopes = value.get("scope")
    if not isinstance(scopes, list):
        errors.append(f"{location}: scope must be an array")
        return
    roles = []
    for index, scoped in enumerate(scopes):
        if not isinstance(scoped, dict) or set(scoped) != {"role", "ref"}:
            errors.append(f"{location}.scope[{index}]: invalid role-labelled reference")
            continue
        role = scoped["role"]
        if not isinstance(role, str) or not role:
            errors.append(f"{location}.scope[{index}]: role must be a non-empty string")
        else:
            roles.append(role)
        validate_ref(scoped["ref"], f"{location}.scope[{index}].ref", errors)
    expect(len(roles) == len(set(roles)), f"{location}: duplicate scope roles", errors)
    if value.get("domain") == "norn" and value.get("kind") == "session_event":
        expect(
            "owning_session_incarnation" in roles,
            f"{location}: Norn event reference lacks its session incarnation",
            errors,
        )


def validate_role_refs(
    values: Any,
    location: str,
    ref_key: str,
    errors: list[str],
) -> None:
    if not isinstance(values, list):
        errors.append(f"{location}: must be an array")
        return
    roles = []
    for index, value in enumerate(values):
        if not isinstance(value, dict) or set(value) != {"role", ref_key}:
            errors.append(f"{location}[{index}]: invalid role-labelled reference")
            continue
        role = value["role"]
        if not isinstance(role, str) or not role:
            errors.append(f"{location}[{index}]: role must be a non-empty string")
        else:
            roles.append(role)
        validate_ref(value[ref_key], f"{location}[{index}].{ref_key}", errors)
    expect(len(roles) == len(set(roles)), f"{location}: duplicate roles", errors)


def validate_event(fixture: dict[str, Any], errors: list[str]) -> None:
    record = fixture["record"]
    required = {
        "record_type",
        "event_id",
        "event_kind",
        "schema_ref",
        "producer_ref",
        "producer_epoch",
        "subjects",
        "direct_causes",
        "links",
        "payload",
    }
    optional = {"actor_ref", "correlation_ref", "occurred_at"}
    expect(
        required <= set(record),
        f"{fixture['fixture_id']}: event fields missing",
        errors,
    )
    expect(
        set(record) <= required | optional,
        f"{fixture['fixture_id']}: unexpected event fields",
        errors,
    )
    for field in ("event_id", "schema_ref", "producer_ref", "producer_epoch"):
        validate_ref(record.get(field), f"{fixture['fixture_id']}.{field}", errors)
    if "actor_ref" in record:
        validate_ref(record["actor_ref"], f"{fixture['fixture_id']}.actor_ref", errors)
    if "correlation_ref" in record:
        validate_ref(
            record["correlation_ref"],
            f"{fixture['fixture_id']}.correlation_ref",
            errors,
        )
    validate_role_refs(
        record.get("subjects"), f"{fixture['fixture_id']}.subjects", "ref", errors
    )
    validate_role_refs(
        record.get("direct_causes"),
        f"{fixture['fixture_id']}.direct_causes",
        "event_ref",
        errors,
    )
    validate_role_refs(
        record.get("links"), f"{fixture['fixture_id']}.links", "ref", errors
    )


def validate_relation(fixture: dict[str, Any], errors: list[str]) -> None:
    record = fixture["record"]
    required = {
        "record_type",
        "relation_id",
        "relation_kind",
        "schema_ref",
        "asserting_producer_ref",
        "asserting_producer_epoch",
        "endpoints",
        "supporting_native_refs",
        "direct_causes",
    }
    optional = {"supersedes_relation_id", "retracts_relation_id", "payload"}
    expect(
        required <= set(record),
        f"{fixture['fixture_id']}: relation fields missing",
        errors,
    )
    expect(
        set(record) <= required | optional,
        f"{fixture['fixture_id']}: unexpected relation fields",
        errors,
    )
    for field in (
        "relation_id",
        "schema_ref",
        "asserting_producer_ref",
        "asserting_producer_epoch",
        "supersedes_relation_id",
        "retracts_relation_id",
    ):
        if field in record:
            validate_ref(record[field], f"{fixture['fixture_id']}.{field}", errors)
    validate_role_refs(
        record.get("endpoints"), f"{fixture['fixture_id']}.endpoints", "ref", errors
    )
    validate_role_refs(
        record.get("direct_causes"),
        f"{fixture['fixture_id']}.direct_causes",
        "event_ref",
        errors,
    )
    supporting = record.get("supporting_native_refs")
    if not isinstance(supporting, list):
        errors.append(
            f"{fixture['fixture_id']}.supporting_native_refs: must be an array"
        )
    else:
        for index, value in enumerate(supporting):
            validate_ref(
                value,
                f"{fixture['fixture_id']}.supporting_native_refs[{index}]",
                errors,
            )


def walk_keys(value: Any) -> list[str]:
    if isinstance(value, dict):
        return list(value) + [
            key for child in value.values() for key in walk_keys(child)
        ]
    if isinstance(value, list):
        return [key for child in value for key in walk_keys(child)]
    return []


def validate_fixture(fixture: dict[str, Any], entry: dict[str, Any]) -> list[str]:
    fixture_id = entry["id"]
    errors: list[str] = []
    required = {
        "fixture_format",
        "fixture_id",
        "status",
        "classification",
        "record",
        "authority_notes",
    }
    optional = {"norn_support"}
    expect(required <= set(fixture), f"{fixture_id}: top-level fields missing", errors)
    expect(
        set(fixture) <= required | optional,
        f"{fixture_id}: unexpected top-level fields",
        errors,
    )
    expect(
        fixture.get("fixture_format") == "ns0-logical-candidate-v1",
        f"{fixture_id}: wrong fixture format",
        errors,
    )
    expect(
        fixture.get("fixture_id") == fixture_id, f"{fixture_id}: id mismatch", errors
    )
    expect(
        fixture.get("status") == "candidate_not_canonical_bytes",
        f"{fixture_id}: fixture overclaims canonical status",
        errors,
    )
    expect(
        fixture.get("classification") == entry["classification"],
        f"{fixture_id}: classification mismatch",
        errors,
    )
    record = fixture.get("record")
    expect(isinstance(record, dict), f"{fixture_id}: record must be an object", errors)
    if not isinstance(record, dict):
        return errors
    expect(
        record.get("record_type") == entry["record_type"],
        f"{fixture_id}: record type mismatch",
        errors,
    )
    forbidden = {
        "record_digest",
        "canonical_bytes",
        "canonical_sha256",
        "authorization_decision",
    }
    expect(
        not (forbidden & set(walk_keys(fixture))),
        f"{fixture_id}: contains a forbidden authority or canonical-byte claim",
        errors,
    )
    notes = fixture.get("authority_notes")
    if not isinstance(notes, dict) or set(notes) != {
        "observed",
        "proposed",
        "owner_decision",
    }:
        errors.append(f"{fixture_id}: authority notes must separate all three states")
    else:
        for state, values in notes.items():
            expect(
                isinstance(values, list)
                and all(isinstance(value, str) for value in values),
                f"{fixture_id}: authority state {state} must be a string array",
                errors,
            )

    if entry["record_type"] == "EventRecordV1":
        validate_event(fixture, errors)
    elif entry["record_type"] == "RelationRecordV1":
        validate_relation(fixture, errors)

    if fixture_id == "opaque-unknown":
        expect(
            set(record)
            == {
                "record_type",
                "native_domain",
                "native_kind",
                "executable",
                "opaque_payload",
            },
            "opaque-unknown: unexpected opaque record fields",
            errors,
        )
        expect(
            record.get("executable") is False,
            "opaque-unknown: unknown record must be non-executable",
            errors,
        )
        payload = record.get("opaque_payload", {})
        expect(
            payload.get("unknown_field") == "must-not-be-dropped",
            "opaque-unknown: sentinel field was not retained",
            errors,
        )
    if fixture_id == "multi-parent-unsupported":
        expect(
            fixture.get("norn_support")
            == "unsupported_by_current_format_2_persistence",
            "multi-parent-unsupported: missing current Norn limitation",
            errors,
        )
        expect(
            len(record.get("direct_causes", [])) == 2,
            "multi-parent-unsupported: expected exactly two direct parents",
            errors,
        )
    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()

    try:
        manifest = load_json(args.manifest)
    except (OSError, ValueError) as error:
        sys.stderr.write(f"manifest: {error}\n")
        return 1

    errors: list[str] = []
    reports = []
    ids: list[str] = []
    for entry in manifest.get("fixtures", []):
        path = args.manifest.parent / entry["path"]
        try:
            fixture = load_json(path)
            digest = file_sha256(path)
            ids.append(entry["id"])
            if digest != entry["sha256"]:
                errors.append(f"{entry['id']}: fixture file hash mismatch")
            errors.extend(validate_fixture(fixture, entry))
            reports.append({"id": entry["id"], "path": entry["path"], "sha256": digest})
        except (OSError, ValueError, KeyError, TypeError) as error:
            errors.append(f"{entry.get('id', '<unknown>')}: {error}")

    expect(len(ids) == 6, "manifest must contain exactly six fixture cases", errors)
    expect(len(ids) == len(set(ids)), "manifest contains duplicate fixture ids", errors)
    report = {
        "format": 1,
        "manifest": str(args.manifest),
        "manifest_sha256": file_sha256(args.manifest),
        "passed": not errors,
        "fixture_count": len(reports),
        "fixtures": reports,
        "errors": errors,
    }
    rendered = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.output is None:
        sys.stdout.write(rendered)
    else:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(rendered, encoding="utf-8")
    return 0 if not errors else 1


if __name__ == "__main__":
    raise SystemExit(main())
