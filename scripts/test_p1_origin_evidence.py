"""Focused tests for exact P1 origin-evidence derivation."""

import copy
import importlib.util
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MODULE_PATH = ROOT / "scripts/p1_origin_evidence.py"
SPEC = importlib.util.spec_from_file_location("p1_origin_evidence", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("cannot load P1 origin evidence module")
EVIDENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EVIDENCE)


class P1OriginEvidenceTests(unittest.TestCase):
    def test_known_snapshot_vector_binds_order_kind_and_bytes(self) -> None:
        entries = [
            (b"a.rs", 0, "100644", "a"),
            (b"z.rs", 1, "120000", "z"),
        ]
        objects = {"a": b"a", "z": b"target"}
        self.assertEqual(
            EVIDENCE.snapshot_identity(entries, objects),
            "d062b92866feb5bfaf16f851d0cef6efdc6ddd6474eb7793e474374aa5f3a58b",
        )
        reversed_entries = list(reversed(entries))
        with self.assertRaises(EVIDENCE.EvidenceError):
            EVIDENCE.snapshot_identity(reversed_entries, objects)

    def test_registry_identity_binds_nested_technical_fields(self) -> None:
        registry = {"schema_version": 1, "entries": [{"output_basename": "a.rs"}]}
        changed = copy.deepcopy(registry)
        changed["entries"][0]["output_basename"] = "b.rs"
        self.assertNotEqual(
            EVIDENCE.registry_identity(registry),
            EVIDENCE.registry_identity(changed),
        )

    def test_retained_evidence_recomputes_from_read_only_git_objects(self) -> None:
        expected = EVIDENCE.decode_strict_json(
            (
                ROOT / "crates/norn-policy/tests/evidence/p1_base_authority.json"
            ).read_text(encoding="utf-8")
        )
        self.assertEqual(EVIDENCE.build_evidence(ROOT), expected)

    def test_retained_evidence_decoder_rejects_duplicate_keys(self) -> None:
        with self.assertRaises(EVIDENCE.EvidenceError):
            EVIDENCE.decode_strict_json('{"schema_version":1,"schema_version":1}')

    def test_retained_evidence_decoder_rejects_non_finite_numbers(self) -> None:
        for value in ["NaN", "Infinity", "-Infinity"]:
            with self.subTest(value=value):
                with self.assertRaises(EVIDENCE.EvidenceError):
                    EVIDENCE.decode_strict_json('{"value":' + value + "}")

    def test_path_validation_rejects_unicode_controls(self) -> None:
        with self.assertRaises(EVIDENCE.EvidenceError):
            EVIDENCE.validate_path("path\u0085name".encode())


if __name__ == "__main__":
    unittest.main()
