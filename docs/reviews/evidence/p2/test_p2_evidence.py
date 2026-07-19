"""Non-live contract tests for the P2 evidence package."""

from __future__ import annotations

import importlib
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


HERE = Path(__file__).resolve().parent
EVIDENCE = HERE.parent
sys.path[:0] = [str(HERE), str(EVIDENCE / "p3-p4"), str(EVIDENCE)]

support = importlib.import_module("p2_evidence_support")
live = importlib.import_module("run_p2_live_aba")
scanner = importlib.import_module("p3_p4_redaction")
p2_redaction = importlib.import_module("p2_redaction")
policy_support = importlib.import_module("p2_policy_support")


class ContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.repo = support.repo_root()
        cls.contract = support.load_contract(
            cls.repo / "docs/reviews/evidence/p2/p2_contract.json"
        )

    def test_claim_inventory_covers_every_p2_finding(self) -> None:
        observed = {
            claim for case in self.contract["phase_cases"] for claim in case["claims"]
        }
        expected = {f"AUTH-{index:02d}" for index in range(1, 8)}
        expected.update({"CONFIG-01", "CONFIG-02"})
        self.assertEqual(observed, expected)

    def test_case_and_distribution_identifiers_are_unique(self) -> None:
        cases = self.contract["phase_cases"]
        self.assertEqual(len({case["id"] for case in cases}), len(cases))
        tests = self.contract["distribution_tests"]
        self.assertEqual(len(set(tests)), len(tests))
        self.assertEqual(self.contract["distribution_runs"] * len(tests), 180)

    def test_every_exact_test_exists_at_the_frozen_source(self) -> None:
        source = self.contract["implementation_source"]
        for case in self.contract["phase_cases"]:
            name = case["test"].rsplit("::", 1)[-1]
            result = support.run(
                ["git", "grep", "-F", f"fn {name}(", source, "--", "crates"],
                cwd=self.repo,
                check=False,
            )
            self.assertEqual(result.returncode, 0, case["id"])
        for test in self.contract["distribution_tests"]:
            name = test.rsplit("::", 1)[-1]
            result = support.run(
                ["git", "grep", "-F", f"fn {name}(", source, "--", "crates"],
                cwd=self.repo,
                check=False,
            )
            self.assertEqual(result.returncode, 0, test)

    def test_selector_listing_requires_the_complete_qualified_identity(self) -> None:
        required = ("module::tests::target_case",)
        with self.assertRaises(RuntimeError):
            support.require_selector_listing(
                b"other_module::tests::target_case: test\n",
                required,
                "fixture:lib",
            )
        self.assertEqual(
            support.require_selector_listing(
                b"module::tests::target_case: test\n",
                required,
                "fixture:lib",
            ),
            1,
        )

    def test_selector_groups_cover_phase_and_distribution_contracts(self) -> None:
        grouped = support.required_selector_groups(self.contract)
        observed = {
            (package, target, test)
            for package, target, tests in grouped
            for test in tests
        }
        expected = {
            (case["package"], case["target"], case["test"])
            for case in self.contract["phase_cases"]
        }
        expected.update(
            ("norn", "lib", test) for test in self.contract["distribution_tests"]
        )
        self.assertEqual(observed, expected)

    def test_selector_attestation_rejects_an_understated_inventory(self) -> None:
        records = [
            {
                "package": package,
                "target": target,
                "required_tests": list(tests),
                "required_test_count": len(tests),
                "listed_test_count": len(tests),
                "result": "pass",
            }
            for package, target, tests in support.required_selector_groups(
                self.contract
            )
        ]
        self.assertEqual(support.selector_inventory_errors(records, self.contract), [])
        records[0]["required_test_count"] = 0
        self.assertTrue(support.selector_inventory_errors(records, self.contract))

    def test_policy_support_is_bound_to_the_evidence_package(self) -> None:
        package = support.commit(self.repo, "HEAD")
        manifest = support.blob_manifest(
            self.repo, package, policy_support.POLICY_SUPPORT
        )
        self.assertEqual(
            [record["path"] for record in manifest],
            list(policy_support.POLICY_SUPPORT),
        )
        self.assertTrue(all(len(record["blob"]) == 40 for record in manifest))

    def test_policy_command_binds_the_detached_repository(self) -> None:
        policy_source = support.target_root(self.repo) / "worktrees" / "fixture"
        output = support.target_root(self.repo) / "evidence" / "fixture.json"
        command = policy_support.command(
            sys.executable, self.repo, policy_source, output, self.contract
        )
        self.assertEqual(
            command[4],
            str(self.repo / "docs/reviews/evidence/run_p0_policy_evidence.py"),
        )
        self.assertEqual(command[-2:], ["--repository", str(policy_source)])

    def test_policy_runner_rejects_a_head_from_another_worktree(self) -> None:
        evidence = support.target_root(self.repo) / "evidence"
        evidence.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="p2-policy-head-test-", dir=evidence
        ) as raw:
            output = Path(raw) / "policy.json"
            result = support.run(
                [
                    sys.executable,
                    "-I",
                    "-S",
                    "-B",
                    str(self.repo / "docs/reviews/evidence/run_p0_policy_evidence.py"),
                    "--base",
                    self.contract["base"],
                    "--head",
                    self.contract["implementation_source"],
                    "--output",
                    str(output),
                    "--repository",
                    str(self.repo),
                ],
                cwd=self.repo,
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(b"HEAD must equal the requested head", result.stdout)
            self.assertFalse(output.exists())

    def test_policy_worktree_stays_below_repository_target(self) -> None:
        target = support.target_root(self.repo)
        with tempfile.TemporaryDirectory(prefix="p2-worktree-test-", dir=target) as raw:
            fake_target = Path(raw)
            with (
                patch.object(support, "target_root", return_value=fake_target),
                patch.object(support, "run") as execute,
            ):
                with support.detached_worktree(
                    self.repo,
                    self.contract["implementation_source"],
                    "fixture",
                ) as worktree:
                    self.assertEqual(worktree.parent, fake_target / "worktrees")
                add = execute.call_args_list[0]
                remove = execute.call_args_list[1]
        self.assertEqual(add.args[0][0:3], ["git", "worktree", "add"])
        self.assertEqual(remove.args[0][0:3], ["git", "worktree", "remove"])

    def test_auth_product_blobs_are_unchanged_at_the_integration_anchor(self) -> None:
        inventory = support.auth_path_inventory(
            self.repo,
            self.contract["base"],
            self.contract["implementation_source"],
            self.contract["integration_anchor"],
            self.contract["integration_anchor"],
        )
        self.assertGreater(inventory["count"], 50)

    def test_historical_artifact_hash_is_pinned(self) -> None:
        for path, expected in self.contract["historical_artifacts"].items():
            data = support.git(
                self.repo, "show", f"{self.contract['implementation_source']}:{path}"
            )
            self.assertEqual(support.sha256(data), expected)

    def test_cleanliness_ignores_ignored_target_but_rejects_untracked_source(
        self,
    ) -> None:
        parent = support.target_root(self.repo)
        with tempfile.TemporaryDirectory(prefix="p2-cleanliness-", dir=parent) as raw:
            fixture = Path(raw)
            support.run(["git", "init", "--quiet"], cwd=fixture)
            (fixture / ".gitignore").write_text("target/\n", encoding="utf-8")
            (fixture / "tracked.txt").write_text("fixture\n", encoding="utf-8")
            support.run(["git", "add", ".gitignore", "tracked.txt"], cwd=fixture)
            support.run(
                [
                    "git",
                    "-c",
                    "user.name=P2 Evidence",
                    "-c",
                    "user.email=p2-evidence@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "fixture",
                ],
                cwd=fixture,
            )
            ignored = fixture / "target" / "cache.bin"
            ignored.parent.mkdir()
            ignored.write_bytes(b"ignored\n")
            support.require_clean_package(fixture)

            (fixture / "source.rs").write_text("fn main() {}\n", encoding="utf-8")
            with self.assertRaises(RuntimeError):
                support.require_clean_package(fixture)

    def test_phase_fixture_and_historical_evidence_inventory_is_redacted(self) -> None:
        report = p2_redaction.build(
            self.repo,
            self.contract["base"],
            self.contract["implementation_source"],
            self.contract["historical_artifacts"],
            {},
            scanner,
            support.git,
        )
        self.assertTrue(report["passed"], report["findings"])


class RedactionTests(unittest.TestCase):
    def assert_rejected(self, value: bytes, rule: str) -> None:
        record = scanner.scan_payload("fixture.json", value, "generated_final", True)
        self.assertFalse(record["passed"])
        self.assertGreater(record["rule_matches"][rule], 0)

    def test_live_evidence_rejects_credential_and_identity_material(self) -> None:
        self.assert_rejected(
            b'{"authorization":"Bearer sk-proj-0123456789ABCDEFGHIJKLMNOP"}\n',
            "credential_material",
        )
        self.assert_rejected(
            b'{"account_id":"123e4567-e89b-12d3-a456-426614174000"}\n',
            "real_account_identifier",
        )

    def test_live_alias_validation_is_non_disclosing(self) -> None:
        with patch.dict(os.environ, {"P2_ALIAS": "fixture-a"}, clear=False):
            self.assertEqual(live.required_alias("P2_ALIAS"), "fixture-a")
        with patch.dict(os.environ, {"P2_ALIAS": "../private"}, clear=False):
            with self.assertRaises(RuntimeError) as raised:
                live.required_alias("P2_ALIAS")
        self.assertNotIn("../private", str(raised.exception))


if __name__ == "__main__":
    unittest.main()
