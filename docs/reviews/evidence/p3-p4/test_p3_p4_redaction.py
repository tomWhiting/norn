"""Focused contract tests for the final P3/P4 redaction gate."""

from __future__ import annotations

import hashlib
import importlib
import json
import sys
import unittest
from pathlib import Path


EVIDENCE_ROOT = Path(__file__).resolve().parent.parent
sys.path[:0] = [str(Path(__file__).resolve().parent), str(EVIDENCE_ROOT)]

redaction = importlib.import_module("p3_p4_redaction")
runner = importlib.import_module("run_p3_p4_final_evidence")
support = importlib.import_module("p3_p4_final_support")


def json_record(value: object, *, historical: bool = False) -> dict[str, object]:
    return redaction.scan_payload(
        "fixture.json",
        (json.dumps(value) + "\n").encode(),
        "retained_historical" if historical else "generated_final",
        not historical,
    )


class RedactionNegativeTests(unittest.TestCase):
    def assert_rejected(self, value: object, rule: str) -> None:
        record = json_record(value)
        self.assertFalse(record["passed"])
        self.assertGreater(record["rule_matches"][rule], 0)

    def test_api_key_is_rejected(self) -> None:
        self.assert_rejected(
            {"authorization": "Bearer sk-proj-0123456789ABCDEFGHIJKLMNOP"},
            "credential_material",
        )

    def test_jwt_is_rejected(self) -> None:
        self.assert_rejected(
            {
                "value": (
                    "eyJhbGciOiJSUzI1NiJ9."
                    "eyJzdWIiOiIxMjM0NTY3ODkwIn0."
                    "abcdefghijklmno123456789"
                )
            },
            "credential_material",
        )

    def test_real_account_identifier_is_rejected(self) -> None:
        self.assert_rejected(
            {"chatgpt-account-id": "123e4567-e89b-12d3-a456-426614174000"},
            "real_account_identifier",
        )

    def test_private_prompt_content_is_rejected(self) -> None:
        self.assert_rejected(
            {"input": "Email the unpublished result to researcher@company.com"},
            "private_prompt_content",
        )

    def test_reusable_turn_state_is_rejected(self) -> None:
        self.assert_rejected(
            {"previous_response_id": "resp_01JABCDEFGHIJKLMNOPQRSTUV"},
            "reusable_turn_state",
        )

    def test_raw_cache_key_is_rejected(self) -> None:
        self.assert_rejected(
            {"prompt_cache_key": "01JABCDEFGHIJKLMNOPQRSTUVWXY"},
            "raw_cache_key",
        )

    def test_short_raw_cache_key_is_rejected(self) -> None:
        self.assert_rejected(
            {"prompt_cache_key": "customer-12345"}, "raw_cache_key"
        )

    def test_generated_absolute_path_is_rejected(self) -> None:
        self.assert_rejected(
            {"target": "/Users/operator/private/target"},
            "absolute_private_path",
        )

    def test_marker_substring_does_not_exempt_an_opaque_bearer(self) -> None:
        self.assert_rejected(
            {"authorization": "Bearer prod_test_0123456789ABCDEF"},
            "credential_material",
        )

    def test_marker_prefix_does_not_exempt_real_material(self) -> None:
        for field, rule in (
            ("authorization", "credential_material"),
            ("api_key", "credential_material"),
            ("previous_response_id", "reusable_turn_state"),
        ):
            with self.subTest(field=field):
                value = (
                    "Bearer test-REALPRODUCTIONTOKEN012345"
                    if field == "authorization"
                    else "test-REALPRODUCTIONTOKEN012345"
                )
                self.assert_rejected({field: value}, rule)

    def test_credential_material_in_a_json_key_is_rejected(self) -> None:
        secret = "sk-proj-0123456789ABCDEFGHIJKLMNOP"
        record = json_record({secret: "header-name"})
        rendered = json.dumps(record, sort_keys=True)

        self.assertFalse(record["passed"])
        self.assertGreater(record["rule_matches"]["credential_material"], 0)
        self.assertNotIn(secret, rendered)
        self.assertNotIn(hashlib.sha256(secret.encode()).hexdigest(), rendered)

    def test_duplicate_json_keys_fail_without_disclosing_values(self) -> None:
        secret = "REALPRODUCTIONSECRET012345"
        record = redaction.scan_payload(
            "fixture.json",
            (
                '{"api_key":"'
                + secret
                + '","api_key":"test-api-key-value"}\n'
            ).encode(),
            "generated_final",
            True,
        )
        rendered = json.dumps(record, sort_keys=True)

        self.assertFalse(record["passed"])
        self.assertEqual(record["rule_matches"]["artifact_integrity"], 1)
        self.assertNotIn(secret, rendered)

    def test_attestation_parse_error_does_not_disclose_duplicate_key(self) -> None:
        secret = "sk-proj-0123456789ABCDEFGHIJKLMNOP"
        payload = ('{"' + secret + '":1,"' + secret + '":2}').encode()

        with self.assertRaises(RuntimeError) as raised:
            runner.strict_document(payload, "fixture", redaction.strict_json_loads)

        self.assertEqual(str(raised.exception), "fixture JSON is invalid")
        self.assertNotIn(secret, str(raised.exception))

    def test_credential_key_does_not_leak_through_child_location(self) -> None:
        secret = "sk-proj-0123456789ABCDEFGHIJKLMNOP"
        record = json_record({secret: "Bearer productiontoken0123456789"})

        self.assertFalse(record["passed"])
        self.assertGreater(record["rule_matches"]["credential_material"], 0)
        rendered = json.dumps(record, sort_keys=True)
        self.assertNotIn(secret, rendered)
        self.assertNotIn(hashlib.sha256(secret.encode()).hexdigest(), rendered)


class RedactionFalsePositiveTests(unittest.TestCase):
    def test_source_names_schema_names_and_urls_are_not_secrets(self) -> None:
        record = json_record(
            {
                "paths": [
                    "crates/norn/src/provider/openai_oauth/credential_state.rs",
                    "crates/norn/src/loop/runner/prompt.rs",
                ],
                "properties": [
                    "prompt_cache_key",
                    "previous_response_id",
                    "chatgpt-account-id",
                ],
                "documentation": "https://platform.openai.com/docs/api-reference/responses",
                "authorization": "Bearer sentinel",
                "account_id": "fixture-account",
            }
        )
        self.assertTrue(record["passed"], record["findings"])

    def test_exact_redaction_rule_identifiers_are_schema_metadata(self) -> None:
        record = json_record(
            {
                "findings": [{"rule": "private_prompt_content"}],
                "rule_matches": {rule: 0 for rule in redaction.RULES},
                "rules": redaction.RULES,
            },
            historical=True,
        )
        self.assertTrue(record["passed"], record["findings"])

    def test_rule_identifier_does_not_exempt_private_content(self) -> None:
        record = json_record(
            {"input": "private_prompt_content: my password is production-secret"}
        )
        self.assertFalse(record["passed"])
        self.assertEqual(record["rule_matches"]["private_prompt_content"], 1)

    def test_reserved_email_and_placeholders_are_allowed(self) -> None:
        record = json_record(
            {
                "input": "Send the synthetic result to agent@example.test",
                "previous_response_id": "fixture-response-id",
                "prompt_cache_key": "redacted-cache-key",
                "api_key": "test-api-key-value",
            }
        )
        self.assertTrue(record["passed"], record["findings"])

    def test_historical_absolute_path_is_counted_but_not_rejected(self) -> None:
        record = json_record(
            {"target": "/Users/operator/private/target"}, historical=True
        )
        self.assertTrue(record["passed"])
        self.assertEqual(record["historical_absolute_path_disclosures"], 1)

    def test_source_fixture_path_names_do_not_trigger_keyword_matches(self) -> None:
        record = redaction.scan_payload(
            "fixture.rs",
            b'const PATH: &str = "crates/norn/src/provider/auth/credential.rs";\n',
            "phase_fixture",
            True,
        )
        self.assertTrue(record["passed"], record["findings"])

    def test_declared_synthetic_fixture_home_is_allowed(self) -> None:
        for path in ("/home/user", "/root", "/tmp", "/tmp/test"):
            with self.subTest(path=path):
                record = redaction.scan_payload(
                    "fixture.rs",
                    f'const OUTPUT: &str = "{path}";\n'.encode(),
                    "phase_fixture",
                    True,
                )
                self.assertTrue(record["passed"], record["findings"])

    def test_real_source_fixture_home_is_rejected(self) -> None:
        record = redaction.scan_payload(
            "fixture.rs",
            b'const OUTPUT: &str = "/Users/tom/private";\n',
            "phase_fixture",
            True,
        )
        self.assertFalse(record["passed"])
        self.assertEqual(record["rule_matches"]["absolute_private_path"], 1)

    def test_private_path_descendants_are_rejected(self) -> None:
        for path in (
            "/home/user/private/passwords",
            "/root/private/passwords",
            "/tmp/private-build",
        ):
            with self.subTest(path=path):
                record = redaction.scan_payload(
                    "fixture.rs",
                    f'const OUTPUT: &str = "{path}";\n'.encode(),
                    "phase_fixture",
                    True,
                )
                self.assertFalse(record["passed"])
                self.assertEqual(
                    record["rule_matches"]["absolute_private_path"], 1
                )

    def test_recorded_python_command_has_no_host_executable_path(self) -> None:
        repo = Path(__file__).resolve().parents[4]
        command = support.command_text(repo, [sys.executable, "evidence.py"])
        self.assertEqual(command, "<python> evidence.py")

    def test_document_validation_requires_exact_passing_evidence(self) -> None:
        expected = {"passed": True, "inventory": []}
        self.assertTrue(redaction.redaction_document_valid(expected, expected))
        self.assertFalse(
            redaction.redaction_document_valid(
                {"passed": True, "inventory": ["extra"]}, expected
            )
        )


if __name__ == "__main__":
    unittest.main()
