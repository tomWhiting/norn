"""Structure, seal, validate, and publish private P1 gate evidence."""

import hashlib
import json
import os
import re
import stat
from pathlib import Path
from typing import Any, Callable

REDACTED = b"[REDACTED]"
PRIVATE_KEY = re.compile(
    rb"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
    re.DOTALL,
)
AUTHORIZATION = re.compile(rb"(?i)(authorization\s*:\s*(?:bearer|basic)\s+)[^\s]+")
NAMED_SECRET = re.compile(
    rb"(?i)((?:api[_-]?key|access[_-]?token|refresh[_-]?token|client[_-]?secret|password)\s*[:=]\s*)[^\s,;]+"
)
TOKEN_SHAPE = re.compile(
    rb"(?<![A-Za-z0-9_-])(?:sk|gh[pousr])[-_][A-Za-z0-9_-]{16,}(?![A-Za-z0-9_-])"
)
JWT_SHAPE = re.compile(
    rb"(?<![A-Za-z0-9_-])[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}(?![A-Za-z0-9_-])"
)
URL_USERINFO = re.compile(rb"(?i)(https?://)[^/@\s:]+:[^/@\s]+@")
EMAIL = re.compile(
    rb"(?i)(?<![A-Za-z0-9._%+-])[A-Za-z0-9._%+-]+@([A-Za-z0-9.-]+\.[A-Za-z]{2,})(?![A-Za-z0-9.-])"
)
PART_NAMES = ("descriptor.json.part", "descriptor.sanitize.part")
SAFE_ID = re.compile(r"^[a-z0-9][a-z0-9-]*$")
FAILED_SUMMARY = b"result=fail\n"


class EvidenceError(Exception):
    """Evidence could not be safely finalized."""


def stable_hash(path: Path) -> tuple[int, str, tuple[int, int, int, int]]:
    before = path.lstat()
    if not stat.S_ISREG(before.st_mode):
        raise EvidenceError("evidence file is not regular")
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    after = path.lstat()
    before_id = (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns)
    after_id = (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns)
    if before_id != after_id:
        raise EvidenceError("evidence file changed while it was hashed")
    return after.st_size, digest.hexdigest(), after_id


def sanitize_bytes(value: bytes, root: Path, home: Path) -> bytes:
    for path, replacement in sorted(
        ((str(root).encode(), b"[REPOSITORY]"), (str(home).encode(), b"[HOME]")),
        key=lambda item: len(item[0]),
        reverse=True,
    ):
        if path:
            value = value.replace(path, replacement)
    value = PRIVATE_KEY.sub(REDACTED, value)
    value = AUTHORIZATION.sub(lambda match: match.group(1) + REDACTED, value)
    value = NAMED_SECRET.sub(lambda match: match.group(1) + REDACTED, value)
    value = TOKEN_SHAPE.sub(REDACTED, value)
    value = JWT_SHAPE.sub(REDACTED, value)
    value = URL_USERINFO.sub(lambda match: match.group(1) + REDACTED + b"@", value)
    value = EMAIL.sub(redact_email, value)
    return value


def redact_email(match: re.Match[bytes]) -> bytes:
    domain = match.group(1).lower()
    if domain == b"example.invalid" or domain.endswith(b".example.invalid"):
        return match.group(0)
    return REDACTED


def assert_sanitized(value: bytes, root: Path, home: Path) -> None:
    if sanitize_bytes(value, root, home) != value:
        raise EvidenceError(
            "evidence still contains confidential or path-bearing content"
        )


def atomic_private_replace(path: Path, value: bytes, part_name: str) -> None:
    part = path.parent / part_name
    try:
        if os.path.lexists(part):
            if part.is_dir() and not part.is_symlink():
                raise EvidenceError("evidence partial path is a directory")
            part.unlink()
        descriptor = os.open(part, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        try:
            with os.fdopen(descriptor, "wb") as output:
                descriptor = -1
                output.write(value)
                output.flush()
                os.fsync(output.fileno())
        except Exception:
            if descriptor >= 0:
                os.close(descriptor)
            raise
        os.replace(part, path)
        sync_directory(path.parent)
    finally:
        if os.path.lexists(part):
            if part.is_dir() and not part.is_symlink():
                raise EvidenceError("evidence partial path is a directory")
            part.unlink()


def sync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def log_record(run_path: Path, path: Path) -> dict[str, Any]:
    size, digest, _identity = stable_hash(path)
    return {
        "path": path.relative_to(run_path).as_posix(),
        "bytes": size,
        "sha256": digest,
    }


def command_stem(record: dict[str, Any]) -> str:
    order = record.get("order")
    identifier = record.get("id")
    if (
        type(order) is not int
        or order < 1
        or not isinstance(identifier, str)
        or SAFE_ID.fullmatch(identifier) is None
    ):
        raise EvidenceError("command identity cannot name retained evidence")
    return f"{order:02d}-{identifier}"


def nonnegative(value: Any, label: str) -> int:
    if type(value) is not int or value < 0:
        raise EvidenceError(f"{label} is not an observed nonnegative integer")
    return value


def structured_log_bytes(record: dict[str, Any], stream: str) -> bytes:
    if stream == "stdout":
        outcome = record.get("outcome")
        if outcome not in {"passed", "failed"}:
            raise EvidenceError("command outcome cannot be summarized")
        fields = [
            f"result={'pass' if outcome == 'passed' else 'fail'}",
            f"tests={nonnegative(record.get('test_executions'), 'test executions')}",
        ]
        distribution = record.get("distribution")
        if distribution is not None:
            if not isinstance(distribution, dict) or set(distribution) != {
                "observations",
                "passed",
                "failed",
            }:
                raise EvidenceError("distribution counts cannot be summarized")
            passed = nonnegative(distribution["passed"], "distribution passed count")
            failed = nonnegative(distribution["failed"], "distribution failed count")
            observations = nonnegative(
                distribution["observations"], "distribution observation count"
            )
            if observations != passed + failed:
                raise EvidenceError("distribution counts do not reconcile")
            fields.extend((f"passed={passed}", f"failed={failed}"))
    elif stream == "stderr":
        process = record.get("process_outcome")
        if process not in {"passed", "failed", "failed_to_start"}:
            raise EvidenceError("process outcome cannot be summarized")
        fields = [f"result={'pass' if process == 'passed' else 'fail'}"]
        exit_code = record.get("exit_code")
        if exit_code is not None:
            if type(exit_code) is not int:
                raise EvidenceError("process exit status cannot be summarized")
            if exit_code >= 0:
                fields.append(f"exit_status={exit_code}")
    else:
        raise EvidenceError("unknown retained stream")
    return (" ".join(fields) + "\n").encode("ascii")


def write_structured_logs(run_path: Path, record: dict[str, Any]) -> None:
    stem = command_stem(record)
    for stream in ("stdout", "stderr"):
        path = run_path / f"{stem}.{stream}.log"
        atomic_private_replace(
            path,
            structured_log_bytes(record, stream),
            f".{path.name}.structured.part",
        )
        path.chmod(0o600)


def refresh_record(run_path: Path, record: dict[str, Any]) -> None:
    stem = command_stem(record)
    for stream in ("stdout", "stderr"):
        record[stream] = log_record(run_path, run_path / f"{stem}.{stream}.log")


def refresh_records(run_path: Path, records: list[dict[str, Any]]) -> None:
    for record in records:
        refresh_record(run_path, record)


def expected_log_names(records: list[dict[str, Any]]) -> set[str]:
    return {
        f"{command_stem(record)}.{stream}.log"
        for record in records
        for stream in ("stdout", "stderr")
    }


def remove_path(path: Path, label: str) -> None:
    if path.is_dir() and not path.is_symlink():
        raise EvidenceError(f"{label} path is a directory")
    path.unlink()


def rewrite_structured_logs(
    run_path: Path, records: list[dict[str, Any]], reject_unexpected: bool
) -> None:
    expected = expected_log_names(records)
    actual = {
        path.name for path in run_path.iterdir() if path.name.endswith(".log")
    }
    unexpected = actual - expected
    for name in sorted(unexpected):
        remove_path(run_path / name, "unexpected retained log")
    for record in records:
        write_structured_logs(run_path, record)
    if reject_unexpected and unexpected:
        raise EvidenceError("retained log inventory contained an unexpected file")


def close_all_logs(run_path: Path) -> None:
    for path in sorted(run_path.iterdir()):
        if not path.name.endswith(".log"):
            continue
        if path.is_dir() and not path.is_symlink():
            raise EvidenceError("retained log path is a directory")
        atomic_private_replace(
            path, FAILED_SUMMARY, f".{path.name}.structured.part"
        )
        path.chmod(0o600)


def distribution_sidecar_name(record: dict[str, Any]) -> str:
    return f"{command_stem(record)}.distribution.json"


def distribution_document(
    root: Path, run_path: Path, record: dict[str, Any]
) -> dict[str, Any]:
    identifier = record["id"]
    stdout = run_path / f"{command_stem(record)}.stdout.log"
    _size, digest, _identity = stable_hash(stdout)
    try:
        referenced_path = stdout.relative_to(root).as_posix()
    except ValueError as error:
        raise EvidenceError("distribution log is outside the repository") from error
    return {
        "schema_version": 1,
        "artifact_family": "distribution",
        "artifact_id": f"p1-distribution-{identifier}",
        "synthetic_values": [],
        "observations": [
            {
                "id": f"distribution-{identifier}-stdout",
                "referenced_path": referenced_path,
                "referenced_family": "sanitized_log",
                "source": "local_gate",
                "synthetic_ids": [],
                "digest": digest,
            }
        ],
    }


def remove_distribution_sidecars(run_path: Path) -> None:
    for path in sorted(run_path.iterdir()):
        if path.name.endswith(".distribution.json"):
            remove_path(path, "distribution sidecar")


def write_distribution_sidecars(
    root: Path, run_path: Path, records: list[dict[str, Any]]
) -> set[str]:
    distribution_records = [
        record for record in records if record.get("kind") == "distribution"
    ]
    expected = {distribution_sidecar_name(record) for record in distribution_records}
    actual = {
        path.name
        for path in run_path.iterdir()
        if path.name.endswith(".distribution.json")
    }
    unexpected = actual - expected
    for name in sorted(unexpected):
        remove_path(run_path / name, "unexpected distribution sidecar")
    for record in distribution_records:
        name = distribution_sidecar_name(record)
        encoded = (
            json.dumps(
                distribution_document(root, run_path, record),
                indent=2,
                sort_keys=True,
            )
            + "\n"
        ).encode("utf-8")
        atomic_private_replace(
            run_path / name, encoded, f".{name}.distribution.part"
        )
        (run_path / name).chmod(0o600)
    if unexpected:
        raise EvidenceError("distribution sidecar inventory contained an unexpected file")
    return expected


def freeze_artifacts(
    run_path: Path, names: set[str]
) -> dict[str, tuple[int, str, tuple[int, int, int, int]]]:
    seals: dict[str, tuple[int, str, tuple[int, int, int, int]]] = {}
    for name in sorted(names):
        path = run_path / name
        if path.is_symlink() or not path.is_file():
            raise EvidenceError("retained evidence is not a regular file")
        path.chmod(0o400)
        seals[name] = stable_hash(path)
    return seals


def verify_seals(
    run_path: Path, seals: dict[str, tuple[int, str, tuple[int, int, int, int]]]
) -> None:
    for name, expected in seals.items():
        if stable_hash(run_path / name) != expected:
            raise EvidenceError("retained evidence changed during finalization")


def cleanup_partials(run_path: Path) -> None:
    for name in PART_NAMES:
        path = run_path / name
        if os.path.lexists(path):
            if path.is_dir() and not path.is_symlink():
                raise EvidenceError("stale evidence partial is a directory")
            path.unlink()
    for path in run_path.iterdir():
        if not path.name.endswith(
            (".sanitize.part", ".structured.part", ".distribution.part")
        ):
            continue
        if path.is_dir() and not path.is_symlink():
            raise EvidenceError("stale sanitization partial is a directory")
        path.unlink()


def reject_run(run_path: Path) -> None:
    descriptor = run_path / "descriptor.json"
    if os.path.lexists(descriptor):
        if descriptor.is_dir() and not descriptor.is_symlink():
            raise EvidenceError("descriptor path is a directory")
        descriptor.unlink()
    remove_distribution_sidecars(run_path)
    for path in run_path.iterdir():
        if path.name.endswith(".log"):
            if path.is_symlink() or not path.is_file():
                raise EvidenceError("rejected log is not a regular file")
            path.chmod(0o400)
    marker = run_path / "REJECTED"
    atomic_private_replace(
        marker, b"p1-gate evidence rejected during finalization\n", ".rejected.part"
    )
    marker.chmod(0o400)
    run_path.chmod(0o500)


def publish_descriptor(
    run_path: Path,
    root: Path,
    home: Path,
    descriptor: dict[str, Any],
    validate: Callable[[], None],
) -> None:
    final = run_path / "descriptor.json"
    partial = run_path / "descriptor.json.part"
    try:
        cleanup_partials(run_path)
        records = descriptor["commands"]
        if not isinstance(records, list):
            raise EvidenceError("descriptor commands must be an array")
        rewrite_structured_logs(run_path, records, reject_unexpected=True)
        refresh_records(run_path, descriptor["commands"])
        sidecars = write_distribution_sidecars(root, run_path, records)
        seals = freeze_artifacts(
            run_path, expected_log_names(records).union(sidecars)
        )
        prepared = (json.dumps(descriptor, indent=2, sort_keys=True) + "\n").encode(
            "utf-8"
        )
        validate()
        encoded = (json.dumps(descriptor, indent=2, sort_keys=True) + "\n").encode(
            "utf-8"
        )
        if encoded != prepared:
            raise EvidenceError("descriptor changed during final validation")
        assert_sanitized(encoded, root, home)
        atomic_private_replace(partial, encoded, "descriptor.sanitize.part")
        verify_seals(run_path, seals)
        os.replace(partial, final)
        sync_directory(run_path)
        if final.read_bytes() != encoded:
            raise EvidenceError("published descriptor differs from validated bytes")
        final.chmod(0o400)
        verify_seals(run_path, seals)
        run_path.chmod(0o500)
    except Exception as error:
        try:
            cleanup_partials(run_path)
            close_all_logs(run_path)
            reject_run(run_path)
        except Exception as rejection_error:
            raise EvidenceError(
                "evidence finalization and rejection both failed"
            ) from rejection_error
        if isinstance(error, EvidenceError):
            raise
        raise EvidenceError("evidence finalization failed") from error
