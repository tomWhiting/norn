"""Non-live contract tests for the P2 evidence package."""

from __future__ import annotations

import importlib
import os
import sys
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


class ContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.repo = support.repo_root()
        cls.contract = support.load_contract(cls.repo / "docs/reviews/evidence/p2/p2_contract.json")

    def test_claim_inventory_covers_every_p2_finding(self) -> None:
        observed = {
            claim
            for case in self.contract["phase_cases"]
            for claim in case["claims"]
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
