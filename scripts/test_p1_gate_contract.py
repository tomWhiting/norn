"""Adversarial tests for P1 manifest and evidence semantics."""

import copy
import importlib.machinery
import importlib.util
import json
import os
import shutil
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MODULE_PATH = ROOT / "scripts/p1_gate_contract.py"
SPEC = importlib.util.spec_from_file_location("p1_gate_contract", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("cannot load P1 gate contract")
CONTRACT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONTRACT)
GATE_SPEC = importlib.util.spec_from_file_location(
    "p1_gate", ROOT / "scripts/p1_gate.py"
)
if GATE_SPEC is None or GATE_SPEC.loader is None:
    raise RuntimeError("cannot load P1 gate orchestrator")
GATE = importlib.util.module_from_spec(GATE_SPEC)
GATE_SPEC.loader.exec_module(GATE)
PHASE_LOCK_FIXTURE = GATE.load_json(
    ROOT / "crates/norn-policy/tests/evidence/p1_phase_lock_parity.json"
)


def load_support(name: str):
    path = ROOT / f"scripts/{name}"
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), str(path))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    if spec is None:
        raise RuntimeError(f"cannot load support entrypoint: {name}")
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


ADDED_AUDIT = load_support("p1-added-line-audit")
REDACTION_CHECK = load_support("p1-redaction-check")
DISTRIBUTIONS = load_support("p1-distributions")
SELF_CHECK = load_support("p1-gate-self-check")


def phase_lock() -> dict[str, object]:
    return copy.deepcopy(PHASE_LOCK_FIXTURE)


def mutated_phase_lock(
    path: tuple[str, ...], replacement: object
) -> dict[str, object]:
    value = phase_lock()
    target = value
    for component in path[:-1]:
        child = target.get(component)
        if not isinstance(child, dict):
            raise AssertionError(f"phase-lock fixture path is not an object: {component}")
        target = child
    target[path[-1]] = replacement
    return value


def snapshot() -> dict[str, object]:
    return {
        "commit": "1" * 40,
        "tree": "2" * 40,
        "clean": True,
        "status_sha256": "3" * 64,
        "conflict_sha256": "4" * 64,
        "submodule_sha256": "5" * 64,
        "submodules_clean": True,
    }


def failed_record(run_path: Path) -> dict[str, object]:
    stdout = run_path / "01-rustc-version.stdout.log"
    stderr = run_path / "01-rustc-version.stderr.log"
    stdout.write_bytes(b"")
    stderr.write_bytes(b"fixed failure\n")
    stdout.chmod(0o600)
    stderr.chmod(0o600)
    return {
        "order": 1,
        "id": "rustc-version",
        "kind": "metadata",
        "argv": ["@rustc", "--version", "--verbose"],
        "tool": {"id": "rustc", "sha256": "a" * 64},
        "started_at": "2026-07-15T00:00:00.000000Z",
        "completed_at": "2026-07-15T00:00:01.000000Z",
        "process_outcome": "failed",
        "outcome": "failed",
        "exit_code": 1,
        "failure_code": "command_exit_nonzero",
        "test_executions": 0,
        "distribution": None,
        "stdout": {
            "path": stdout.name,
            "bytes": stdout.stat().st_size,
            "sha256": CONTRACT.sha256_file(stdout),
        },
        "stderr": {
            "path": stderr.name,
            "bytes": stderr.stat().st_size,
            "sha256": CONTRACT.sha256_file(stderr),
        },
    }


def descriptor(run_path: Path) -> tuple[dict[str, object], dict[str, object]]:
    start = snapshot()
    value = {
        "outcome": "failed",
        "failure_codes": ["command_exit_nonzero:rustc-version"],
        "candidate": {"commit": start["commit"], "tree": start["tree"]},
        "base": {"commit": "6" * 40, "tree": "7" * 40},
        "gate": {"identity": "fixed"},
        "interpreter": {"id": "python", "sha256": "8" * 64, "version": "3.11.0"},
        "environment": {"caller_environment_inherited": []},
        "resource_limits": {
            "status": "owner_decision_required",
            "command_timeout_seconds": None,
            "per_log_max_bytes": None,
        },
        "tools": [{"id": "rustc", "sha256": "a" * 64}],
        "repository_start": start,
        "commands": [failed_record(run_path)],
        "repository_end": copy.deepcopy(start),
    }
    expected = {
        field: copy.deepcopy(value[field])
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
    return value, expected


class GateContractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.run_path = (
            ROOT / "target/p1-gate-tests" / f"contract-{os.getpid()}-{id(self)}"
        )
        self.run_path.mkdir(parents=True)
        self.manifest = json.loads(
            (ROOT / "policy/gate-commands.json").read_text(encoding="utf-8")
        )

    def tearDown(self) -> None:
        shutil.rmtree(self.run_path)

    def assert_invalid(
        self,
        value: dict[str, object],
        expected: dict[str, object],
    ) -> None:
        with self.assertRaises(CONTRACT.ContractError):
            CONTRACT.validate_descriptor(value, self.manifest, self.run_path, expected)

    def test_failed_prefix_descriptor_is_semantically_valid(self) -> None:
        value, expected = descriptor(self.run_path)
        CONTRACT.validate_descriptor(value, self.manifest, self.run_path, expected)

    def test_manifest_has_fixed_logical_executables_and_active_legs(self) -> None:
        pins = CONTRACT.validate_manifest(self.manifest)
        self.assertEqual(pins["distributions"]["status"], "active")
        self.assertEqual(self.manifest["commands"][0]["argv"][0], "@rustc")

    def test_manifest_pins_every_current_gate_file(self) -> None:
        pins = CONTRACT.validate_manifest(self.manifest)
        CONTRACT.verify_pinned_files(ROOT, pins)

    def test_phase_lock_accepts_the_complete_format_bound_authority(self) -> None:
        GATE.validate_phase_lock(phase_lock())

    def test_shared_phase_lock_fixture_rejects_fixed_identity_drift(self) -> None:
        mutations = (
            (("schema_version",), 2),
            (("active_phase",), "P2"),
            (("base", "object_format"), "sha256"),
            (("base", "commit"), "0" * 40),
            (("base", "tree"), "0" * 40),
            (("algorithms", "analyzer"), "norn-policy-2"),
            (
                ("algorithms", "digest"),
                "norn-sha256-canonical-json-2",
            ),
            (("digests", "generated_include_registry"), "9" * 64),
            (("digests", "governance_anchor"), "9" * 64),
            (("digests", "repository_policy"), "invalid"),
            (("gate", "entrypoint_path"), "scripts/other-gate"),
            (("gate", "command_manifest_path"), "policy/other-commands.json"),
            (("gate", "entrypoint_sha256"), "invalid"),
        )
        for path, replacement in mutations:
            with self.subTest(path=path):
                with self.assertRaises(GATE.GateError):
                    GATE.validate_phase_lock(mutated_phase_lock(path, replacement))

    def test_phase_lock_rejects_missing_or_unknown_base_fields(self) -> None:
        for field in ("object_format", "commit", "tree"):
            value = phase_lock()
            del value["base"][field]
            with self.assertRaises(GATE.GateError):
                GATE.validate_phase_lock(value)
        value = phase_lock()
        value["base"]["alternate"] = True
        with self.assertRaises(GATE.GateError):
            GATE.validate_phase_lock(value)

    def test_phase_lock_rejects_wrong_object_format(self) -> None:
        value = phase_lock()
        value["base"]["object_format"] = "sha256"
        with self.assertRaises(GATE.GateError):
            GATE.validate_phase_lock(value)

    def test_phase_lock_requires_the_exact_generated_registry_authority(self) -> None:
        for replacement in (None, "9" * 64):
            value = phase_lock()
            if replacement is None:
                del value["digests"]["generated_include_registry"]
            else:
                value["digests"]["generated_include_registry"] = replacement
            with self.assertRaises(GATE.GateError):
                GATE.validate_phase_lock(value)

    def test_phase_lock_requires_the_exact_governance_anchor(self) -> None:
        for replacement in (None, "9" * 64):
            value = phase_lock()
            if replacement is None:
                del value["digests"]["governance_anchor"]
            else:
                value["digests"]["governance_anchor"] = replacement
            with self.assertRaises(GATE.GateError):
                GATE.validate_phase_lock(value)

    def test_candidate_must_equal_start_identity(self) -> None:
        value, expected = descriptor(self.run_path)
        value["candidate"] = {"commit": "9" * 40, "tree": "2" * 40}
        expected["candidate"] = copy.deepcopy(value["candidate"])
        self.assert_invalid(value, expected)

    def test_command_argv_is_bound_to_manifest(self) -> None:
        value, expected = descriptor(self.run_path)
        value["commands"][0]["argv"] = ["@rustc", "--version"]
        self.assert_invalid(value, expected)

    def test_command_tool_digest_is_bound_to_inventory(self) -> None:
        value, expected = descriptor(self.run_path)
        value["commands"][0]["tool"]["sha256"] = "b" * 64
        self.assert_invalid(value, expected)

    def test_log_bytes_are_rehashed(self) -> None:
        value, expected = descriptor(self.run_path)
        with (self.run_path / "01-rustc-version.stderr.log").open("ab") as output:
            output.write(b"late mutation\n")
        self.assert_invalid(value, expected)

    def test_unreferenced_log_is_rejected(self) -> None:
        value, expected = descriptor(self.run_path)
        extra = self.run_path / "orphan.log"
        extra.write_bytes(b"orphan")
        extra.chmod(0o600)
        self.assert_invalid(value, expected)

    def test_world_readable_log_is_rejected(self) -> None:
        value, expected = descriptor(self.run_path)
        (self.run_path / "01-rustc-version.stdout.log").chmod(0o644)
        self.assert_invalid(value, expected)

    def test_distribution_success_requires_twenty_clean_observations(self) -> None:
        record = {
            "kind": "distribution",
            "outcome": "passed",
            "failure_code": None,
            "distribution": {"observations": 19, "passed": 19, "failed": 0},
        }
        with self.assertRaises(CONTRACT.ContractError):
            CONTRACT.validate_distribution_relation(record)
        record["distribution"] = {"observations": 20, "passed": 19, "failed": 1}
        with self.assertRaises(CONTRACT.ContractError):
            CONTRACT.validate_distribution_relation(record)
        record["distribution"] = {"observations": 20, "passed": 20, "failed": 0}
        CONTRACT.validate_distribution_relation(record)

    def test_distribution_counts_do_not_override_process_failure(self) -> None:
        record = {
            "process_outcome": "failed",
            "exit_code": 3,
            "outcome": "failed",
            "failure_code": "command_exit_nonzero",
            "kind": "distribution",
            "distribution": {"observations": 20, "passed": 20, "failed": 0},
        }
        CONTRACT.validate_process_relation(record)
        CONTRACT.validate_distribution_relation(record)

    def test_tool_records_must_be_unique_and_sorted(self) -> None:
        with self.assertRaises(CONTRACT.ContractError):
            CONTRACT.validate_tool_records(
                [
                    {"id": "rustc", "sha256": "a" * 64},
                    {"id": "cargo", "sha256": "b" * 64},
                ]
            )


class GateSupportEntrypointTests(unittest.TestCase):
    @staticmethod
    def patch(*lines: str) -> bytes:
        body = "\n".join(f"+{line}" for line in lines)
        return (
            "diff --git a/sample.rs b/sample.rs\n"
            "new file mode 100644\n"
            "--- /dev/null\n"
            "+++ b/sample.rs\n"
            f"@@ -0,0 +1,{len(lines)} @@\n{body}\n"
        ).encode()

    def test_added_line_audit_binds_the_exact_phase_diff(self) -> None:
        self.assertEqual(
            ADDED_AUDIT.DIFF_ARGUMENTS,
            (
                "diff",
                "--no-ext-diff",
                "--unified=0",
                "2917c8ed10e7a2ec7ac9c4d7283bafbea7f6577d...HEAD",
            ),
        )

    def test_added_line_audit_rejects_every_closed_category(self) -> None:
        examples = {
            "prohibited_calls": ".unw" + "rap()",
            "prohibited_macros": "pa" + "nic!()",
            "lint_attributes": "#[al" + "low(dead_code)]",
            "test_exclusions": "#[ig" + "nore]",
            "external_suppressions": "# no" + "qa",
            "debt_markers": "# TO" + "DO",
            "lint_reductions": "RUST" + "FLAGS=-A warnings",
            "hidden_test_forms": "#[cfg(any" + "())]",
        }
        for category, line in examples.items():
            with self.subTest(category=category):
                _added, counts = ADDED_AUDIT.scan_diff(self.patch(line))
                self.assertGreater(counts[category], 0)

    def test_added_line_audit_accepts_ordinary_additions(self) -> None:
        added, counts = ADDED_AUDIT.scan_diff(
            self.patch("fn checked() -> bool {", "    true", "}")
        )
        self.assertEqual(added, 3)
        self.assertFalse(any(counts.values()))

    def test_added_line_audit_rejects_uninspectable_binary_diff(self) -> None:
        with self.assertRaises(ADDED_AUDIT.AuditError):
            ADDED_AUDIT.scan_diff(
                b"diff --git a/value b/value\nBinary files a/value and b/value differ\n"
            )

    @staticmethod
    def ready_policy() -> bytes:
        return json.dumps(
            {
                "state": "ready",
                "value": {
                    "source_inventory": "a" * 64,
                    "findings": [],
                    "legacy_dispositions": [],
                },
            },
            separators=(",", ":"),
        ).encode()

    def test_redaction_check_requires_one_clear_ready_policy_report(self) -> None:
        REDACTION_CHECK.validate_policy_output(self.ready_policy())
        with self.assertRaises(REDACTION_CHECK.RedactionCheckError):
            REDACTION_CHECK.validate_policy_output(b'{"state":"absent"}')
        with self.assertRaises(REDACTION_CHECK.RedactionCheckError):
            REDACTION_CHECK.validate_policy_output(
                b'{"state":"ready","state":"ready","value":{}}'
            )

    def test_distribution_requires_the_exact_unskipped_test_result(self) -> None:
        output = (
            f"running 1 test\ntest {DISTRIBUTIONS.TEST_NAME} ... ok\n\n"
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; "
            "436 filtered out; finished in 0.01s\n"
        ).encode()
        passed = subprocess.CompletedProcess([], 0, output, b"")
        skipped = subprocess.CompletedProcess(
            [],
            0,
            output.replace(
                b"1 passed; 0 failed; 0 ignored",
                b"0 passed; 0 failed; 1 ignored",
            ),
            b"",
        )
        self.assertEqual(DISTRIBUTIONS.OBSERVATIONS, 20)
        self.assertTrue(DISTRIBUTIONS.observation_passed(passed))
        self.assertFalse(DISTRIBUTIONS.observation_passed(skipped))

    def test_self_check_reconciles_compiled_and_discovered_tool_inventory(self) -> None:
        compiled = SELF_CHECK.parse_compiled_paths(
            SELF_CHECK.PATH_POLICY.read_text(encoding="utf-8")
        )
        self.assertEqual(compiled, SELF_CHECK.candidate_paths())
        self.assertEqual(len(compiled), 29)
        SELF_CHECK.validate_python_dependencies(compiled)
        self.assertEqual(len(SELF_CHECK.checked_test_paths(compiled)), 8)

    def test_self_check_rejects_any_pending_manifest_row(self) -> None:
        active = [{"status": "active"}]
        SELF_CHECK.require_active(active)
        with self.assertRaises(SELF_CHECK.SelfCheckError):
            SELF_CHECK.require_active([{"status": "pending_fail_closed"}])


if __name__ == "__main__":
    unittest.main()
