"""Adversarial finalization tests for private P1 gate evidence."""

import hashlib
import importlib.util
import json
import os
import re
import shutil
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MODULE_PATH = ROOT / "scripts/p1_gate_evidence.py"
SPEC = importlib.util.spec_from_file_location("p1_gate_evidence", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("cannot load P1 gate evidence module")
EVIDENCE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EVIDENCE)
GATE_SPEC = importlib.util.spec_from_file_location("p1_gate", ROOT / "scripts/p1_gate.py")
if GATE_SPEC is None or GATE_SPEC.loader is None:
    raise RuntimeError("cannot load P1 gate module")
GATE = importlib.util.module_from_spec(GATE_SPEC)
GATE_SPEC.loader.exec_module(GATE)


def record(
    kind: str = "metadata", distribution: dict[str, int] | None = None
) -> dict[str, object]:
    return {
        "order": 1,
        "id": "sample",
        "kind": kind,
        "process_outcome": "passed",
        "outcome": "passed",
        "exit_code": 0,
        "test_executions": 3,
        "distribution": distribution,
        "stdout": {"path": "placeholder", "bytes": 0, "sha256": "0" * 64},
        "stderr": {"path": "placeholder", "bytes": 0, "sha256": "0" * 64},
    }


def private_email_sentinel() -> bytes:
    return b"person" + b"@" + b"real" + b"." + b"test"


class GateEvidenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.test_root = (
            ROOT
            / "target/p1-gate/evidence"
            / f"test-{os.getpid()}-{id(self)}"
        )
        self.run_path = self.test_root / "run"
        self.run_path.mkdir(parents=True)
        self.stdout = self.run_path / "01-sample.stdout.log"
        self.stderr = self.run_path / "01-sample.stderr.log"
        self.stdout.write_bytes(b"ordinary output\n")
        self.stderr.write_bytes(b"")
        self.stdout.chmod(0o600)
        self.stderr.chmod(0o600)

    def tearDown(self) -> None:
        if not self.test_root.exists():
            return
        for directory, _children, files in os.walk(self.test_root):
            Path(directory).chmod(0o700)
            for name in files:
                path = Path(directory) / name
                if not path.is_symlink():
                    path.chmod(0o600)
        shutil.rmtree(self.test_root)

    def descriptor(self) -> dict[str, object]:
        return {"schema_version": 1, "commands": [record()], "outcome": "failed"}

    def test_run_identifier_is_accepted_by_lowercase_evidence_path_policy(self) -> None:
        class Environment:
            @staticmethod
            def ensure_private_directory(root: Path, path: Path) -> None:
                if not path.is_relative_to(root):
                    raise ValueError("test directory escaped repository")
                path.mkdir(parents=True, exist_ok=True)

        run_path, run_id = GATE.create_run_directory(
            ROOT, Environment, self.test_root.name
        )
        self.assertEqual(run_path.name, run_id)
        self.assertIsNotNone(re.fullmatch(r"run-[0-9]{8}t[0-9]{6}\.[0-9]{6}z", run_id))
        self.assertEqual(run_id, run_id.lower())

    def test_publish_replaces_process_output_with_structured_logs(self) -> None:
        secret = b"sk-" + b"A" * 24
        private_email = private_email_sentinel()
        raw = secret + b" " + str(ROOT).encode() + b" " + private_email + b"\n"
        self.stdout.write_bytes(raw)
        descriptor = self.descriptor()
        EVIDENCE.publish_descriptor(
            self.run_path, ROOT, ROOT.parent, descriptor, lambda: None
        )
        retained = self.stdout.read_bytes()
        self.assertEqual(retained, b"result=pass tests=3\n")
        self.assertEqual(self.stderr.read_bytes(), b"result=pass exit_status=0\n")
        self.assertNotIn(secret, retained)
        self.assertNotIn(str(ROOT).encode(), retained)
        self.assertNotIn(private_email, retained)
        self.assertEqual(self.stdout.stat().st_mode & 0o777, 0o400)
        self.assertEqual(
            (self.run_path / "descriptor.json").stat().st_mode & 0o777, 0o400
        )
        self.assertEqual(self.run_path.stat().st_mode & 0o777, 0o500)
        published = json.loads(
            (self.run_path / "descriptor.json").read_text(encoding="utf-8")
        )
        self.assertNotIn(
            hashlib.sha256(raw).hexdigest(),
            (self.run_path / "descriptor.json").read_text(encoding="utf-8"),
        )
        self.assertEqual(
            published["commands"][0]["stdout"]["sha256"],
            EVIDENCE.stable_hash(self.stdout)[1],
        )
        self.assertEqual(
            published["commands"][0]["stdout"]["bytes"], len(retained)
        )
        self.assertFalse(
            any(
                path.name.endswith(".distribution.json")
                for path in self.run_path.iterdir()
            )
        )

    def test_distribution_sidecar_binds_final_stdout_log(self) -> None:
        counts = {"observations": 20, "passed": 20, "failed": 0}
        descriptor = {
            "schema_version": 1,
            "commands": [record("distribution", counts)],
            "outcome": "passed",
        }
        EVIDENCE.publish_descriptor(
            self.run_path, ROOT, ROOT.parent, descriptor, lambda: None
        )
        sidecar = self.run_path / "01-sample.distribution.json"
        document = json.loads(sidecar.read_text(encoding="utf-8"))
        stdout = self.run_path / "01-sample.stdout.log"
        self.assertEqual(
            document,
            {
                "schema_version": 1,
                "artifact_family": "distribution",
                "artifact_id": "p1-distribution-sample",
                "synthetic_values": [],
                "observations": [
                    {
                        "id": "distribution-sample-stdout",
                        "referenced_path": stdout.relative_to(ROOT).as_posix(),
                        "referenced_family": "sanitized_log",
                        "source": "local_gate",
                        "synthetic_ids": [],
                        "digest": EVIDENCE.stable_hash(stdout)[1],
                    }
                ],
            },
        )
        self.assertEqual(sidecar.stat().st_mode & 0o777, 0o400)
        published = json.loads(
            (self.run_path / "descriptor.json").read_text(encoding="utf-8")
        )
        self.assertEqual(
            published["commands"][0]["stdout"]["sha256"],
            document["observations"][0]["digest"],
        )

    def test_failed_distribution_still_emits_one_bound_sidecar(self) -> None:
        failed = record("distribution")
        failed["outcome"] = "failed"
        descriptor = {
            "schema_version": 1,
            "commands": [failed],
            "outcome": "failed",
        }
        EVIDENCE.publish_descriptor(
            self.run_path, ROOT, ROOT.parent, descriptor, lambda: None
        )
        stdout = self.run_path / "01-sample.stdout.log"
        sidecar = self.run_path / "01-sample.distribution.json"
        document = json.loads(sidecar.read_text(encoding="utf-8"))
        self.assertEqual(stdout.read_bytes(), b"result=fail tests=3\n")
        self.assertEqual(len(document["observations"]), 1)
        self.assertEqual(
            document["observations"][0]["digest"],
            EVIDENCE.stable_hash(stdout)[1],
        )

    def test_reserved_example_email_is_not_redacted(self) -> None:
        value = b"fixture@example.invalid"
        self.assertEqual(EVIDENCE.sanitize_bytes(value, ROOT, ROOT.parent), value)

    def test_log_mutation_after_seal_rejects_publication(self) -> None:
        descriptor = self.descriptor()

        def mutate() -> None:
            self.stdout.chmod(0o600)
            with self.stdout.open("ab") as output:
                output.write(b"late mutation\n")

        with self.assertRaises(EVIDENCE.EvidenceError):
            EVIDENCE.publish_descriptor(
                self.run_path, ROOT, ROOT.parent, descriptor, mutate
            )
        self.assertFalse((self.run_path / "descriptor.json").exists())
        self.assertTrue((self.run_path / "REJECTED").is_file())
        self.assertEqual(self.stdout.read_bytes(), b"result=fail\n")
        self.assertFalse(
            any(path.name.endswith(".part") for path in self.run_path.iterdir())
        )

    def test_descriptor_mutation_during_validation_rejects_sidecar_binding(self) -> None:
        counts = {"observations": 20, "passed": 20, "failed": 0}
        descriptor = {
            "schema_version": 1,
            "commands": [record("distribution", counts)],
            "outcome": "passed",
        }

        def mutate() -> None:
            descriptor["commands"][0]["test_executions"] = 4

        with self.assertRaises(EVIDENCE.EvidenceError):
            EVIDENCE.publish_descriptor(
                self.run_path, ROOT, ROOT.parent, descriptor, mutate
            )
        self.assertFalse((self.run_path / "descriptor.json").exists())
        self.assertFalse((self.run_path / "01-sample.distribution.json").exists())
        self.assertEqual(self.stdout.read_bytes(), b"result=fail\n")

    def test_descriptor_path_leak_rejects_publication(self) -> None:
        descriptor = self.descriptor()
        descriptor["leak"] = str(ROOT)
        with self.assertRaises(EVIDENCE.EvidenceError):
            EVIDENCE.publish_descriptor(
                self.run_path, ROOT, ROOT.parent, descriptor, lambda: None
            )
        self.assertFalse((self.run_path / "descriptor.json").exists())
        self.assertTrue((self.run_path / "REJECTED").exists())
        self.assertFalse(
            any(path.name.endswith(".part") for path in self.run_path.iterdir())
        )

    def test_validator_failure_removes_sidecar_and_closes_rejected_logs(self) -> None:
        secret = b"ghp_" + b"B" * 24
        self.stderr.write_bytes(secret)
        sidecar = self.run_path / "01-sample.distribution.json"
        saw_sidecar: list[bool] = []

        def reject() -> None:
            saw_sidecar.append(sidecar.is_file())
            raise ValueError("private validator detail")

        counts = {"observations": 20, "passed": 20, "failed": 0}
        descriptor = {
            "schema_version": 1,
            "commands": [record("distribution", counts)],
            "outcome": "passed",
        }
        with self.assertRaises(EVIDENCE.EvidenceError):
            EVIDENCE.publish_descriptor(
                self.run_path, ROOT, ROOT.parent, descriptor, reject
            )
        self.assertEqual(saw_sidecar, [True])
        self.assertFalse(sidecar.exists())
        self.assertNotIn(secret, self.stderr.read_bytes())
        self.assertEqual(self.stderr.read_bytes(), b"result=fail\n")
        self.assertEqual(
            (self.run_path / "REJECTED").read_text(encoding="utf-8"),
            "p1-gate evidence rejected during finalization\n",
        )
        self.assertFalse(
            any(path.name.endswith(".part") for path in self.run_path.iterdir())
        )

    def test_rejection_removes_sidecar_and_unexpected_raw_log(self) -> None:
        secret = b"access_token=" + b"Q" * 24
        unexpected = self.run_path / "99-injected.stdout.log"
        unexpected.write_bytes(secret)
        unexpected.chmod(0o600)
        counts = {"observations": 20, "passed": 20, "failed": 0}
        descriptor = {
            "schema_version": 1,
            "commands": [record("distribution", counts)],
            "outcome": "passed",
        }
        with self.assertRaises(EVIDENCE.EvidenceError):
            EVIDENCE.publish_descriptor(
                self.run_path, ROOT, ROOT.parent, descriptor, lambda: None
            )
        self.assertFalse(unexpected.exists())
        self.assertFalse((self.run_path / "01-sample.distribution.json").exists())
        self.assertNotIn(
            secret,
            b"".join(
                path.read_bytes()
                for path in self.run_path.iterdir()
                if path.is_file()
            ),
        )


if __name__ == "__main__":
    unittest.main()
