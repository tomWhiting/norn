"""Deterministic local P1 gate orchestration."""

import hashlib
import importlib.util
import json
import re
import subprocess
import sys
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

PHASE = "P1"
ANALYZER_VERSION = "norn-policy-1"
DIGEST_VERSION = "norn-sha256-canonical-json-1"
BASE_COMMIT = "2917c8ed10e7a2ec7ac9c4d7283bafbea7f6577d"
BASE_TREE = "9ae969792c53b4e1dfdc61c6d91f7fe62d3ac582"
BASE_OBJECT_FORMAT = "sha1"
GENERATED_INCLUDE_REGISTRY = (
    "5272fe6d23419c9bb42892c55a625bb8b3f00490f9214d19481c45923a7a2e65"
)
GOVERNANCE_ANCHOR_IDENTITY = (
    "e7fbb74ce8863bee999572d1cc5ab9e8668ddac098651bea495323fbff8d061e"
)
ENTRYPOINT_PATH = "scripts/p1-gate"
IMPLEMENTATION_PATH = "scripts/p1_gate.py"
CONTRACT_PATH = "scripts/p1_gate_contract.py"
MANIFEST_PATH = "policy/gate-commands.json"
SCHEMA_PATH = "policy/evidence-schemas/gate-run.schema.json"
PHASE_LOCK_PATH = "policy/phase-lock.json"
HEX_40 = re.compile(r"^[0-9a-f]{40}$")
HEX_64 = re.compile(r"^[0-9a-f]{64}$")


class GateError(Exception):
    """A deterministic gate precondition or evidence error."""


def utc_now() -> str:
    return datetime.now(UTC).isoformat(timespec="microseconds").replace("+00:00", "Z")


def strict_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise GateError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def reject_constant(value: str) -> None:
    raise GateError(f"non-standard JSON constant: {value}")


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=strict_object,
            parse_constant=reject_constant,
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise GateError(
            f"cannot read strict JSON: {path.name}: {type(error).__name__}"
        ) from error
    if not isinstance(value, dict):
        raise GateError(f"JSON root must be an object: {path.name}")
    return value


def exact_keys(value: dict[str, Any], expected: set[str], label: str) -> None:
    if set(value) != expected:
        raise GateError(f"{label} has unknown or missing fields")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def load_module(name: str, path: Path) -> Any:
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise GateError(f"cannot load checked-in gate module: {name}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def bootstrap_pin(
    root: Path,
    manifest: dict[str, Any],
    identifier: str,
    expected_path: str,
) -> None:
    try:
        files = manifest["implementation"]["files"]
        entries = [
            entry
            for entry in files
            if isinstance(entry, dict) and entry.get("id") == identifier
        ]
        if len(entries) != 1 or entries[0].get("path") != expected_path:
            raise GateError(f"command manifest does not identify the gate {identifier}")
        digest = entries[0].get("sha256")
        if not isinstance(digest, str) or not HEX_64.fullmatch(digest):
            raise GateError(f"command manifest {identifier} digest is invalid")
        if sha256_file(root / expected_path) != digest:
            raise GateError(f"gate {identifier} hash does not match command manifest")
    except (KeyError, TypeError) as error:
        raise GateError("command manifest bootstrap shape is invalid") from error


def bootstrap_contract(root: Path, manifest: dict[str, Any]) -> Any:
    bootstrap_pin(root, manifest, "orchestrator", IMPLEMENTATION_PATH)
    bootstrap_pin(root, manifest, "contract", CONTRACT_PATH)
    return load_module("norn_p1_gate_contract", root / CONTRACT_PATH)


def validate_phase_lock(lock: dict[str, Any]) -> None:
    exact_keys(
        lock,
        {"schema_version", "active_phase", "base", "algorithms", "digests", "gate"},
        "phase lock",
    )
    if (
        type(lock["schema_version"]) is not int
        or lock["schema_version"] != 1
        or lock["active_phase"] != PHASE
    ):
        raise GateError("phase lock identity is invalid")
    for field in ("base", "algorithms", "digests", "gate"):
        if not isinstance(lock[field], dict):
            raise GateError(f"phase lock {field} must be an object")
    exact_keys(lock["base"], {"object_format", "commit", "tree"}, "phase lock base")
    exact_keys(lock["algorithms"], {"analyzer", "digest"}, "phase lock algorithms")
    exact_keys(
        lock["digests"],
        {
            "repository_policy",
            "governance",
            "governance_anchor",
            "writer_resolutions",
            "writer_families",
            "generated_include_registry",
            "contract_manifest",
            "evidence_schemas",
            "source_findings",
            "origin",
        },
        "phase lock digests",
    )
    exact_keys(
        lock["gate"],
        {
            "entrypoint_path",
            "entrypoint_sha256",
            "command_manifest_path",
            "command_manifest_sha256",
        },
        "phase lock gate",
    )
    if lock["base"] != {
        "object_format": BASE_OBJECT_FORMAT,
        "commit": BASE_COMMIT,
        "tree": BASE_TREE,
    }:
        raise GateError("phase lock base does not match the ratified P1 base")
    if lock["algorithms"] != {"analyzer": ANALYZER_VERSION, "digest": DIGEST_VERSION}:
        raise GateError("phase lock algorithm identities are invalid")
    if not all(
        isinstance(value, str) and HEX_64.fullmatch(value)
        for value in lock["digests"].values()
    ):
        raise GateError("phase lock contains an invalid digest")
    if lock["digests"]["generated_include_registry"] != GENERATED_INCLUDE_REGISTRY:
        raise GateError("phase lock generated-include registry is invalid")
    if lock["digests"]["governance_anchor"] != GOVERNANCE_ANCHOR_IDENTITY:
        raise GateError("phase lock governance anchor is invalid")
    gate = lock["gate"]
    if (
        gate["entrypoint_path"] != ENTRYPOINT_PATH
        or gate["command_manifest_path"] != MANIFEST_PATH
    ):
        raise GateError("phase lock gate paths are invalid")
    if not all(
        isinstance(value, str) and HEX_64.fullmatch(value)
        for value in (gate["entrypoint_sha256"], gate["command_manifest_sha256"])
    ):
        raise GateError("phase lock gate hashes are invalid")


def run_git(
    root: Path,
    environment: dict[str, str],
    git: Path,
    *arguments: str,
    check: bool = True,
) -> subprocess.CompletedProcess[bytes]:
    result = subprocess.run(
        [str(git), *arguments],
        cwd=root,
        env=environment,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and result.returncode != 0:
        raise GateError("Git precondition command failed: " + " ".join(arguments))
    return result


def git_text(
    root: Path, environment: dict[str, str], git: Path, *arguments: str
) -> str:
    result = run_git(root, environment, git, *arguments)
    try:
        return result.stdout.decode("ascii").strip()
    except UnicodeDecodeError as error:
        raise GateError("Git returned non-ASCII identity data") from error


def repository_snapshot(
    root: Path, environment: dict[str, str], git: Path
) -> dict[str, Any]:
    commit = git_text(root, environment, git, "rev-parse", "--verify", "HEAD^{commit}")
    tree = git_text(root, environment, git, "rev-parse", "--verify", "HEAD^{tree}")
    status = run_git(
        root,
        environment,
        git,
        "status",
        "--porcelain=v2",
        "--untracked-files=all",
        "--ignore-submodules=none",
    ).stdout
    conflicts = run_git(root, environment, git, "ls-files", "--unmerged").stdout
    submodules = run_git(
        root, environment, git, "submodule", "status", "--recursive"
    ).stdout
    return {
        "commit": commit,
        "tree": tree,
        "clean": not status and not conflicts,
        "status_sha256": sha256_bytes(status),
        "conflict_sha256": sha256_bytes(conflicts),
        "submodule_sha256": sha256_bytes(submodules),
        "submodules_clean": all(line[:1] == b" " for line in submodules.splitlines()),
    }


def create_run_directory(root: Path, environment_module: Any, candidate: str) -> tuple[Path, str]:
    candidate_root = root / "target/p1-gate/evidence" / candidate
    environment_module.ensure_private_directory(root, candidate_root)
    stem = datetime.now(UTC).strftime("run-%Y%m%dt%H%M%S.%fz")
    suffix = 0
    while True:
        name = stem if suffix == 0 else f"{stem}-{suffix}"
        run_path = candidate_root / name
        try:
            run_path.mkdir(mode=0o700)
            return run_path, name
        except FileExistsError:
            suffix += 1


def interpreter_record(tools: dict[str, Any]) -> dict[str, str]:
    if sys.version_info < (3, 11) or sys.flags.isolated != 1:
        raise GateError("implementation requires isolated Python 3.11 or newer")
    digest = next(
        record["sha256"] for record in tools["records"] if record["id"] == "python"
    )
    version = f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"
    return {"id": "python", "sha256": digest, "version": version}


def gate_record(lock: dict[str, Any], manifest: dict[str, Any]) -> dict[str, Any]:
    return {
        "entrypoint_path": ENTRYPOINT_PATH,
        "entrypoint_sha256": lock["gate"]["entrypoint_sha256"],
        "command_manifest_path": MANIFEST_PATH,
        "command_manifest_sha256": lock["gate"]["command_manifest_sha256"],
        "pinned_files": manifest["implementation"]["files"],
    }


def main() -> int:
    if len(sys.argv) != 1:
        raise GateError("p1-gate accepts no arguments")
    root = Path(__file__).resolve().parent.parent
    if Path.cwd().resolve() != root or not (root / ".git").exists():
        raise GateError("p1-gate must run from the repository root")
    for relative in (
        ENTRYPOINT_PATH,
        IMPLEMENTATION_PATH,
        CONTRACT_PATH,
        MANIFEST_PATH,
        SCHEMA_PATH,
        PHASE_LOCK_PATH,
    ):
        path = root / relative
        if path.is_symlink() or not path.is_file():
            raise GateError(f"required gate input is not a regular file: {relative}")
    lock = load_json(root / PHASE_LOCK_PATH)
    validate_phase_lock(lock)
    if sha256_file(root / ENTRYPOINT_PATH) != lock["gate"]["entrypoint_sha256"]:
        raise GateError("gate entrypoint hash does not match phase lock")
    if sha256_file(root / MANIFEST_PATH) != lock["gate"]["command_manifest_sha256"]:
        raise GateError("command manifest hash does not match phase lock")
    manifest = load_json(root / MANIFEST_PATH)
    contract = bootstrap_contract(root, manifest)
    pins = contract.validate_manifest(manifest)
    contract.verify_pinned_files(root, pins)
    environment_module = load_module(
        "norn_p1_gate_environment", root / pins["environment"]["path"]
    )
    environment_module.private_umask()
    environment_module.validate_launcher_environment(root)
    evidence = load_module("norn_p1_gate_evidence", root / pins["evidence"]["path"])
    runtime = load_module("norn_p1_gate_runtime", root / pins["runtime"]["path"])
    validator = load_module(
        "norn_p1_json_schema", root / pins["schema-validator"]["path"]
    )
    schema = validator.load_json(root / SCHEMA_PATH)
    validator.validate_schema(schema)
    tools = environment_module.resolve_tools(root, pins)
    interpreter = interpreter_record(tools)
    sdk_id, sdk_path = environment_module.selected_sdk()
    preflight_environment, _preflight_record = (
        environment_module.controlled_environment(
            root, "preflight", tools, sdk_path, sdk_id
        )
    )
    git = tools["paths"]["git"]
    start_snapshot = repository_snapshot(root, preflight_environment, git)
    if not HEX_40.fullmatch(start_snapshot["commit"]) or not HEX_40.fullmatch(
        start_snapshot["tree"]
    ):
        raise GateError("candidate Git identities are invalid")
    if not start_snapshot["clean"] or not start_snapshot["submodules_clean"]:
        raise GateError("checkout or submodules are not clean and initialized")
    actual_base_tree = git_text(
        root,
        preflight_environment,
        git,
        "rev-parse",
        "--verify",
        f"{BASE_COMMIT}^{{tree}}",
    )
    if actual_base_tree != BASE_TREE:
        raise GateError("ratified P1 base tree does not match Git")
    ancestry = run_git(
        root,
        preflight_environment,
        git,
        "merge-base",
        "--is-ancestor",
        BASE_COMMIT,
        start_snapshot["commit"],
        check=False,
    )
    if ancestry.returncode != 0:
        raise GateError("ratified P1 base is not an ancestor of the candidate")
    limits = manifest["resource_limits"]
    if limits["status"] != "ratified":
        raise GateError(
            "command timeout and retained-log size limits require an owner decision"
        )
    run_path, run_id = create_run_directory(
        root, environment_module, start_snapshot["commit"]
    )
    environment, environment_record = environment_module.controlled_environment(
        root, run_id, tools, sdk_path, sdk_id
    )
    tool_records = {record["id"]: record for record in tools["records"]}
    started_at = utc_now()
    records: list[dict[str, Any]] = []
    failures: list[str] = []
    internal_failure: str | None = None
    try:
        for order, command in enumerate(manifest["commands"], start=1):
            print(
                f"[{order:02d}/{len(manifest['commands']):02d}] {command['id']}",
                flush=True,
            )
            identifier = contract.tool_id(command["argv"][0])
            prefix = environment_module.command_prefix(command["argv"][0], tools)
            record, failure = runtime.execute_command(
                root,
                run_path,
                environment,
                order,
                command,
                prefix,
                tool_records[identifier],
                evidence,
            )
            records.append(record)
            if failure is not None:
                failures.append(failure)
    except Exception as error:
        internal_failure = type(error).__name__
        failures.append("gate_internal_error")
    try:
        end_snapshot = repository_snapshot(root, environment, git)
    except Exception as error:
        end_snapshot = {
            "commit": start_snapshot["commit"],
            "tree": start_snapshot["tree"],
            "clean": False,
            "status_sha256": sha256_bytes(b"snapshot failed"),
            "conflict_sha256": sha256_bytes(b"snapshot failed"),
            "submodule_sha256": sha256_bytes(b"snapshot failed"),
            "submodules_clean": False,
        }
        internal_failure = internal_failure or type(error).__name__
        failures.append("final_snapshot_failed")
    if end_snapshot != start_snapshot:
        failures.append("repository_changed_during_gate")
    failures = list(dict.fromkeys(failures))
    outcome = "passed" if not failures else "failed"
    descriptor = {
        "schema_version": 1,
        "evidence_id": "p1-gate-local-001",
        "phase": PHASE,
        "started_at": started_at,
        "completed_at": utc_now(),
        "outcome": outcome,
        "failure_codes": failures,
        "candidate": {
            "commit": start_snapshot["commit"],
            "tree": start_snapshot["tree"],
        },
        "base": {"commit": BASE_COMMIT, "tree": BASE_TREE},
        "gate": gate_record(lock, manifest),
        "interpreter": interpreter,
        "environment": environment_record,
        "resource_limits": limits,
        "tools": tools["records"],
        "repository_start": start_snapshot,
        "commands": records,
        "repository_end": end_snapshot,
    }
    expected = {
        field: descriptor[field]
        for field in (
            "candidate",
            "base",
            "gate",
            "interpreter",
            "environment",
            "tools",
            "repository_start",
            "repository_end",
        )
    }

    def validate_final() -> None:
        contract.verify_pinned_files(root, pins)
        environment_module.verify_tools(tools)
        environment_module.verify_cargo_isolation(
            Path(environment["CARGO_HOME"]),
            tools["home"],
            environment_record["cache_bridges"],
        )
        if sha256_file(root / ENTRYPOINT_PATH) != lock["gate"]["entrypoint_sha256"]:
            raise GateError("gate entrypoint changed during execution")
        if sha256_file(root / MANIFEST_PATH) != lock["gate"]["command_manifest_sha256"]:
            raise GateError("command manifest changed during execution")
        validator.validate_instance(descriptor, schema, schema)
        contract.validate_descriptor(descriptor, manifest, run_path, expected)

    evidence.publish_descriptor(
        run_path, root, tools["home"], descriptor, validate_final
    )
    if internal_failure is not None:
        print(f"p1-gate internal failure: {internal_failure}", file=sys.stderr)
    print(f"p1-gate {outcome}; evidence: {run_path.relative_to(root)}", flush=True)
    return 0 if outcome == "passed" else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except GateError as error:
        print(f"p1-gate rejected: {error}", file=sys.stderr)
        raise SystemExit(2) from None
    except Exception as error:
        print(f"p1-gate internal failure: {type(error).__name__}", file=sys.stderr)
        raise SystemExit(2) from None
