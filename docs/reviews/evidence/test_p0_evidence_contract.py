"""Tamper tests for the exact, path-free P0 evidence schema."""

from __future__ import annotations

import io
import unittest
from dataclasses import replace
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import p0_evidence_attestation_contract as contract
import p0_evidence_disclosure as disclosure
import p0_evidence_gate_cases as gate_cases
import p0_evidence_metadata_contract as metadata_contract
import p0_evidence_paths as paths
import p0_evidence_policy as policy_contract
import p0_evidence_support as support
import p0_evidence_toolchain as toolchain_support
import p0_policy_rust as policy_rust


SHA256 = "0" * 64


def valid_metadata() -> dict[str, object]:
    executables = {
        name: {
            "sha256": (
                SHA256
                if name in {"cargo", "git", "python", "rustc", "rustup"}
                else None
            )
        }
        for name in support.EXECUTABLE_NAMES
    }
    return {
        "schema_version": 3,
        "kind": "distributions",
        "generated_at_utc": "2026-07-14T00:00:00+00:00",
        "base": "41ea210",
        "base_commit": SHA256[:40],
        "head": "1" * 40,
        "worktree_clean": True,
        "platform_system": "Darwin",
        "platform": "macOS-test",
        "python": "3.14.0",
        "rustc": "rustc 1.94.0 (test)",
        "cargo": "cargo 1.94.0 (test)",
        "environment": {
            **support.PATH_FREE_ENVIRONMENT_CONTROLS,
            "sanitized_variable_names": list(support.SANITIZED_VARIABLE_NAMES),
            "removed_ambient_variable_count": 5,
        },
        "environment_fingerprint": {"executables": executables},
        "cargo_config": {
            "repository": {"present": False, "sha256": None},
            "user": {"present": False, "sha256": None},
        },
        "temporary_filesystem": {"filesystem": "apfs"},
        "logical_cpu_count": 8,
        "storage_layout": paths.STORAGE_LAYOUT,
        "cases": [],
        "passed": 0,
        "failed": 0,
        "runner_observations": 0,
        "rust_test_executions": 0,
        "final_repository_state": {"head": "1" * 40, "worktree_clean": True},
        "repository_integrity_passed": True,
    }


def failure_fields(*groups: dict[str, object], complete: bool) -> dict[str, object]:
    names = [name for group in groups for name in group["names"]]
    return {
        "failed_test_names": names,
        "failed_test_identity_groups": list(groups),
        "failed_test_identity_complete": complete,
    }


class MetadataContractTests(unittest.TestCase):
    def test_compiler_fingerprints_use_actual_pinned_binaries(self) -> None:
        def digest(path: Path) -> str:
            return {"cargo": "1" * 64, "rustc": "2" * 64}.get(path.name, "3" * 64)

        with (
            patch.object(
                toolchain_support,
                "pinned_binary",
                side_effect=lambda _environment, _toolchain, name: Path(
                    f"/actual/{name}"
                ),
            ) as resolver,
            patch.object(toolchain_support, "_resolved_digest", return_value=SHA256),
            patch.object(toolchain_support, "sha256_file", side_effect=digest),
        ):
            fingerprint = toolchain_support.environment_fingerprint({}, "1.94.0")

        executables = fingerprint["executables"]
        self.assertEqual(executables["cargo"]["sha256"], "1" * 64)
        self.assertEqual(executables["rustc"]["sha256"], "2" * 64)
        self.assertEqual(executables["rustup"]["sha256"], SHA256)
        self.assertNotEqual(
            executables["cargo"]["sha256"], executables["rustup"]["sha256"]
        )
        self.assertEqual(resolver.call_count, 2)

    def test_valid_metadata_has_exact_disclosure_safe_shape(self) -> None:
        self.assertEqual(
            metadata_contract.metadata_errors(
                valid_metadata(), "distributions", "artifact"
            ),
            [],
        )

    def test_unknown_top_level_key_is_rejected(self) -> None:
        payload = valid_metadata()
        payload["operator_name"] = "private"

        errors = metadata_contract.metadata_errors(payload, "distributions", "artifact")

        self.assertTrue(any("unexpected keys" in error for error in errors))

    def test_absolute_path_anywhere_in_payload_is_rejected(self) -> None:
        payload = valid_metadata()
        payload["platform"] = "/Users/operator/private/tool"

        errors = metadata_contract.metadata_errors(payload, "distributions", "artifact")

        self.assertTrue(any("absolute path" in error for error in errors))

    def test_embedded_unix_path_is_rejected(self) -> None:
        for path in (
            "cargo 1.94.0 (/Users/operator/private)",
            "cargo path:/Users/operator/private",
            "cargo [/Users/operator/private]",
        ):
            with self.subTest(path=path):
                payload = valid_metadata()
                payload["cargo"] = path

                errors = metadata_contract.metadata_errors(
                    payload, "distributions", "artifact"
                )

                self.assertTrue(any("absolute path" in error for error in errors))

    def test_embedded_windows_and_unc_paths_are_rejected(self) -> None:
        for path in (r"tool C:\\private\\cargo.exe", r"tool \\\\host\\share\\cargo"):
            with self.subTest(path=path):
                payload = valid_metadata()
                payload["cargo"] = path

                errors = metadata_contract.metadata_errors(
                    payload, "distributions", "artifact"
                )

                self.assertTrue(any("absolute path" in error for error in errors))

    def test_embedded_file_uri_is_rejected(self) -> None:
        payload = valid_metadata()
        payload["cargo"] = "resolved from file:///Users/operator/private"

        errors = metadata_contract.metadata_errors(payload, "distributions", "artifact")

        self.assertTrue(any("absolute path" in error for error in errors))
        self.assertFalse(
            disclosure.string_has_absolute_path(
                "source https://example.test/path crates/norn/src/lib.rs"
            )
        )

    def test_executable_path_field_is_rejected_even_with_valid_digest(self) -> None:
        payload = valid_metadata()
        executables = payload["environment_fingerprint"]["executables"]
        executables["cargo"]["path"] = "/Users/operator/.cargo/bin/cargo"

        errors = metadata_contract.metadata_errors(payload, "distributions", "artifact")

        self.assertTrue(any("fingerprint shape" in error for error in errors))
        self.assertTrue(any("absolute path" in error for error in errors))

    def test_ambient_variable_name_cannot_be_added(self) -> None:
        payload = valid_metadata()
        payload["environment"]["PRIVATE_API_KEY"] = "removed"

        errors = metadata_contract.metadata_errors(payload, "distributions", "artifact")

        self.assertTrue(any("unexpected keys" in error for error in errors))


class FailureIdentityContractTests(unittest.TestCase):
    def test_duplicate_name_across_separate_binaries_is_valid(self) -> None:
        groups = (
            {
                "source": "summary",
                "declared_failed": 1,
                "target": {"package": "alpha", "kind": "lib", "name": None},
                "names": ["shared::tests::same"],
            },
            {
                "source": "summary",
                "declared_failed": 1,
                "target": {
                    "package": "beta",
                    "kind": "test",
                    "name": "integration",
                },
                "names": ["shared::tests::same"],
            },
        )
        observation = failure_fields(*groups, complete=True)

        errors = contract.failure_identity_errors(
            observation, {"passed": 0, "failed": 2, "ignored": 0}, "run"
        )

        self.assertEqual(errors, [])

    def test_duplicate_name_within_one_binary_is_rejected(self) -> None:
        group = {
            "source": "summary",
            "declared_failed": 2,
            "target": {"package": "norn", "kind": "lib", "name": None},
            "names": ["crate::tests::same", "crate::tests::same"],
        }
        observation = failure_fields(group, complete=False)

        errors = contract.failure_identity_errors(
            observation, {"passed": 0, "failed": 2, "ignored": 0}, "run"
        )

        self.assertTrue(any("duplicate names" in error for error in errors))

    def test_truncated_status_identity_is_retained_without_false_completeness(
        self,
    ) -> None:
        group = {
            "source": "status_fallback",
            "declared_failed": None,
            "target": None,
            "names": ["crate::tests::failed"],
        }
        observation = failure_fields(group, complete=False)

        errors = contract.failure_identity_errors(observation, None, "run")

        self.assertTrue(any("lack a failed-test count" in error for error in errors))

    def test_extra_failure_group_field_is_rejected(self) -> None:
        group = {
            "source": "summary",
            "declared_failed": 1,
            "target": {"package": "norn", "kind": "lib", "name": None},
            "names": ["crate::tests::failed"],
            "diagnostic": "private",
        }
        observation = failure_fields(group, complete=True)

        errors = contract.failure_identity_errors(
            observation, {"passed": 0, "failed": 1, "ignored": 0}, "run"
        )

        self.assertTrue(any("unexpected keys" in error for error in errors))


class CaseContractTests(unittest.TestCase):
    def test_missing_or_extra_self_test_module_is_rejected(self) -> None:
        expected = gate_cases.manifest.EVIDENCE_SELF_TEST_MODULES
        gate_cases.require_self_test_modules(expected)
        with self.assertRaisesRegex(RuntimeError, "module inventory changed"):
            gate_cases.require_self_test_modules(expected[:-1])
        with self.assertRaisesRegex(RuntimeError, "module inventory changed"):
            gate_cases.require_self_test_modules((*expected, "test_p0_unpinned.py"))

    def test_recorded_command_requires_path_free_placeholder(self) -> None:
        unsafe = support.Case("unsafe", "policy", ("/usr/bin/python3", "script.py"))
        with self.assertRaisesRegex(RuntimeError, "absolute recorded command"):
            support.case_contract_command(unsafe)

        embedded = replace(
            unsafe,
            command=("python3", "--output=/Users/operator/private.json"),
        )
        with self.assertRaisesRegex(RuntimeError, "absolute recorded command"):
            support.case_contract_command(embedded)

        windows = replace(unsafe, command=("python", r"C:\private\script.py"))
        with self.assertRaisesRegex(RuntimeError, "absolute recorded command"):
            support.case_contract_command(windows)

        wrapped = replace(unsafe, command=("cargo", "1.94.0 (/Users/operator/private)"))
        with self.assertRaisesRegex(RuntimeError, "absolute recorded command"):
            support.case_contract_command(wrapped)

        safe = replace(unsafe, recorded_command=("<python>", "script.py"))
        self.assertEqual(support.case_contract_command(safe), ["<python>", "script.py"])

    def test_truncated_failure_status_cannot_produce_a_passing_record(self) -> None:
        case = support.Case("test", "tests", ("fake-test-command",))
        completed = SimpleNamespace(
            returncode=0,
            stdout="test crate::tests::failed ... FAILED\n",
            stderr="",
        )

        with (
            patch.object(support.subprocess, "run", return_value=completed),
            patch.object(support.sys, "stderr", io.StringIO()),
        ):
            record = support.run_once(Path("."), {}, case)

        self.assertEqual(record["failed_test_names"], ["crate::tests::failed"])
        self.assertFalse(record["failed_test_identity_complete"])
        self.assertEqual(record["contract_failure"], "unexpected_failure_identity")
        self.assertFalse(record["passed"])

    def test_zero_discovered_tool_tests_cannot_pass(self) -> None:
        case = support.Case(
            "tool-tests",
            "policy",
            ("python", "-m", "unittest"),
            expected_tool_tests=30,
            expected_tool_test_modules=("test_contract.py",),
        )
        completed = SimpleNamespace(
            returncode=0,
            stdout="",
            stderr="Ran 0 tests in 0.000s\n\nOK\n",
        )

        with (
            patch.object(support.subprocess, "run", return_value=completed),
            patch.object(support.sys, "stderr", io.StringIO()),
        ):
            record = support.run_once(Path("."), {}, case)

        self.assertEqual(record["tool_test_count"], 0)
        self.assertEqual(record["contract_failure"], "exact_tool_test_count")
        self.assertFalse(record["passed"])

    def test_exact_tool_test_count_passes(self) -> None:
        case = support.Case(
            "tool-tests",
            "policy",
            ("python", "-m", "unittest"),
            expected_tool_tests=30,
            expected_tool_test_modules=("test_contract.py",),
        )
        completed = SimpleNamespace(
            returncode=0,
            stdout="",
            stderr="Ran 30 tests in 0.010s\n\nOK\n",
        )

        with patch.object(support.subprocess, "run", return_value=completed):
            record = support.run_once(Path("."), {}, case)

        self.assertEqual(record["tool_test_count"], 30)
        self.assertTrue(record["passed"])


class PolicyContractTests(unittest.TestCase):
    def test_policy_read_failure_uses_a_path_free_error_code(self) -> None:
        result, passed = policy_contract.bind_policy_artifact(
            Path("/Users/operator/private/policy.json"),
            Path("."),
            "1" * 40,
            "0" * 40,
        )

        self.assertEqual(result, {"error": "policy_read_failed"})
        self.assertFalse(passed)
        self.assertFalse(disclosure.contains_absolute_path(result))


class PolicyRustCfgTests(unittest.TestCase):
    def test_non_item_test_cfg_is_retained_conservatively(self) -> None:
        evidence_root = Path(__file__).resolve().parent
        repository = evidence_root.parents[2]
        source = evidence_root / "fixtures/cfg_test/non_item.rs"
        rule = evidence_root / "p0-rust-items.yml"

        ranges, _modules = policy_rust.test_only_ranges(repository, rule, source)
        stripped = policy_rust.strip_ranges(source.read_text(encoding="utf-8"), ranges)

        self.assertIn("test_only_statement();", stripped)
        self.assertIn("trailing_test_only_statement();", stripped)
        self.assertNotIn("fn test_only_item", stripped)


if __name__ == "__main__":
    unittest.main()
