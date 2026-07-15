"""Adversarial tests for the dependency-free P1 evidence validator."""

import copy
import importlib.util
import json
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VALIDATOR_PATH = ROOT / "scripts/p1_json_schema.py"
SPEC = importlib.util.spec_from_file_location("p1_json_schema", VALIDATOR_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("cannot load P1 JSON schema validator")
VALIDATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VALIDATOR)


def repository_snapshot() -> dict[str, object]:
    return {
        "commit": "1" * 40,
        "tree": "2" * 40,
        "clean": True,
        "status_sha256": "3" * 64,
        "conflict_sha256": "4" * 64,
        "submodule_sha256": "5" * 64,
        "submodules_clean": True,
    }


def pinned_files() -> list[dict[str, str]]:
    return [
        {
            "id": f"pin-{index}",
            "path": f"scripts/pin-{index}",
            "sha256": format(index, "064x"),
            "status": "active",
        }
        for index in range(16)
    ]


def controlled_environment() -> dict[str, object]:
    return {
        "CARGO_BUILD_JOBS": "1",
        "CARGO_HOME": "target/p1-gate/cargo-home",
        "CARGO_INCREMENTAL": "0",
        "CARGO_NET_OFFLINE": "true",
        "CARGO_TARGET_DIR": "target/p1-gate/cargo-target",
        "CARGO_TERM_COLOR": "never",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_SYSTEM": "/dev/null",
        "GIT_PAGER": "cat",
        "GIT_TERMINAL_PROMPT": "0",
        "HOME": "target/p1-gate/runtime/run/home",
        "LANG": "C",
        "LC_ALL": "C",
        "NO_COLOR": "1",
        "PAGER": "cat",
        "PATH": ["selected-rust-toolchain", "system-usr-bin", "system-bin"],
        "PYTHONDONTWRITEBYTECODE": "1",
        "RUST_BACKTRACE": "0",
        "RUSTC": "tool:rustc",
        "RUSTDOC": "tool:rustdoc",
        "SDKROOT": None,
        "TERM": "dumb",
        "TMPDIR": "target/p1-gate/runtime/run/tmp",
        "TZ": "UTC",
    }


def valid_descriptor() -> dict[str, object]:
    return {
        "schema_version": 1,
        "evidence_id": "p1-gate-local-001",
        "phase": "P1",
        "started_at": "2026-07-15T00:00:00.000000Z",
        "completed_at": "2026-07-15T00:00:01.000000Z",
        "outcome": "failed",
        "failure_codes": ["command_failed:test"],
        "candidate": {"commit": "1" * 40, "tree": "2" * 40},
        "base": {"commit": "6" * 40, "tree": "7" * 40},
        "gate": {
            "entrypoint_path": "scripts/p1-gate",
            "entrypoint_sha256": "8" * 64,
            "command_manifest_path": "policy/gate-commands.json",
            "command_manifest_sha256": "9" * 64,
            "pinned_files": pinned_files(),
        },
        "interpreter": {"id": "python", "sha256": "a" * 64, "version": "3.11.0"},
        "environment": {
            "caller_environment_inherited": [],
            "credential_environment_inherited": [],
            "cache_bridges": ["cargo-registry-cache"],
            "controlled": controlled_environment(),
        },
        "resource_limits": {
            "status": "owner_decision_required",
            "command_timeout_seconds": None,
            "per_log_max_bytes": None,
        },
        "tools": [{"id": "cargo", "sha256": "b" * 64}],
        "repository_start": repository_snapshot(),
        "commands": [],
        "repository_end": repository_snapshot(),
    }


def valid_command() -> dict[str, object]:
    return {
        "order": 1,
        "id": "distributions",
        "kind": "distribution",
        "argv": ["@support:p1-distributions"],
        "tool": {"id": "p1-distributions", "sha256": "c" * 64},
        "started_at": "2026-07-15T00:00:00.000000Z",
        "completed_at": "2026-07-15T00:00:01.000000Z",
        "process_outcome": "passed",
        "outcome": "passed",
        "exit_code": 0,
        "failure_code": None,
        "test_executions": 20,
        "distribution": {"observations": 20, "passed": 20, "failed": 0},
        "stdout": {
            "path": "01-distributions.stdout.log",
            "bytes": 0,
            "sha256": "d" * 64,
        },
        "stderr": {
            "path": "01-distributions.stderr.log",
            "bytes": 0,
            "sha256": "e" * 64,
        },
    }


class EvidenceSchemaTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.schema = VALIDATOR.load_json(
            ROOT / "policy/evidence-schemas/gate-run.schema.json"
        )
        VALIDATOR.validate_schema(cls.schema)

    def assert_invalid(self, descriptor: dict[str, object]) -> None:
        with self.assertRaises(VALIDATOR.ValidationError):
            VALIDATOR.validate_instance(descriptor, self.schema, self.schema)

    def test_complete_failed_descriptor_is_valid(self) -> None:
        VALIDATOR.validate_instance(valid_descriptor(), self.schema, self.schema)

    def test_unknown_descriptor_field_is_rejected(self) -> None:
        descriptor = valid_descriptor()
        descriptor["unregistered"] = True
        self.assert_invalid(descriptor)

    def test_failed_descriptor_requires_a_failure_code(self) -> None:
        descriptor = valid_descriptor()
        descriptor["failure_codes"] = []
        self.assert_invalid(descriptor)

    def test_passed_descriptor_requires_every_command(self) -> None:
        descriptor = valid_descriptor()
        descriptor["outcome"] = "passed"
        descriptor["failure_codes"] = []
        self.assert_invalid(descriptor)

    def test_integer_fields_reject_booleans(self) -> None:
        descriptor = valid_descriptor()
        descriptor["schema_version"] = True
        self.assert_invalid(descriptor)

    def test_gate_pinned_file_inventory_is_fixed_length(self) -> None:
        descriptor = valid_descriptor()
        descriptor["gate"]["pinned_files"].pop()
        self.assert_invalid(descriptor)

    def test_repository_relative_paths_reject_parent_escape(self) -> None:
        descriptor = valid_descriptor()
        descriptor["environment"]["controlled"]["TMPDIR"] = "target/../outside"
        self.assert_invalid(descriptor)

    def test_unratified_limits_must_remain_null(self) -> None:
        descriptor = valid_descriptor()
        descriptor["resource_limits"]["command_timeout_seconds"] = 10
        self.assert_invalid(descriptor)

    def test_passed_distribution_requires_twenty_observations(self) -> None:
        command = valid_command()
        command["distribution"] = {"observations": 19, "passed": 19, "failed": 0}
        with self.assertRaises(VALIDATOR.ValidationError):
            VALIDATOR.validate_instance(
                command, self.schema["$defs"]["command"], self.schema
            )

    def test_passed_process_requires_exit_zero(self) -> None:
        command = valid_command()
        command["exit_code"] = 7
        with self.assertRaises(VALIDATOR.ValidationError):
            VALIDATOR.validate_instance(
                command, self.schema["$defs"]["command"], self.schema
            )

    def test_schema_rejects_unknown_dialect_keywords(self) -> None:
        schema = copy.deepcopy(self.schema)
        schema["format"] = "custom"
        with self.assertRaises(VALIDATOR.ValidationError):
            VALIDATOR.validate_schema(schema)

    def test_strict_json_rejects_duplicate_keys(self) -> None:
        with self.assertRaises(VALIDATOR.ValidationError):
            json.loads('{"same":1,"same":2}', object_pairs_hook=VALIDATOR.strict_object)


if __name__ == "__main__":
    unittest.main()
