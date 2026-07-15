"""Adversarial tests for P1 command execution records."""

import importlib.util
import os
import shutil
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def load_module(name: str, relative: str):
    path = ROOT / relative
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {name}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


RUNTIME = load_module("p1_gate_runtime", "scripts/p1_gate_runtime.py")
EVIDENCE = load_module("p1_gate_evidence", "scripts/p1_gate_evidence.py")


class GateRuntimeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.run_path = (
            ROOT / "target/p1-gate-tests" / f"runtime-{os.getpid()}-{id(self)}"
        )
        self.run_path.mkdir(parents=True)
        self.program = self.run_path / "runner"
        self.environment = {"LANG": "C", "LC_ALL": "C", "PATH": "/usr/bin:/bin"}
        self.tool = {"id": "p1-distributions", "sha256": "a" * 64}

    def tearDown(self) -> None:
        shutil.rmtree(self.run_path)

    def install_program(self, output: str, exit_code: int, stderr: str = "") -> None:
        self.program.write_text(
            "#!/bin/sh\n"
            f"printf '%s\\n' '{output}'\n"
            f"printf '%s\\n' '{stderr}' >&2\n"
            f"exit {exit_code}\n",
            encoding="utf-8",
        )
        self.program.chmod(0o700)

    def command(self, kind: str = "distribution") -> dict[str, object]:
        return {
            "id": "distributions",
            "kind": kind,
            "argv": ["@support:p1-distributions"],
        }

    def execute(self, kind: str = "distribution"):
        return RUNTIME.execute_command(
            ROOT,
            self.run_path,
            self.environment,
            1,
            self.command(kind),
            [str(self.program)],
            self.tool,
            EVIDENCE,
        )

    def test_distribution_requires_at_least_twenty_passes(self) -> None:
        self.install_program(
            '{"schema_version":1,"observations":19,"passed":19,"failed":0}', 0
        )
        record, failure = self.execute()
        self.assertEqual(record["process_outcome"], "passed")
        self.assertEqual(record["outcome"], "failed")
        self.assertEqual(record["failure_code"], "distribution_requirements_failed")
        self.assertEqual(failure, "distribution_requirements_failed:distributions")

    def test_twenty_clean_observations_pass(self) -> None:
        self.install_program(
            '{"schema_version":1,"observations":20,"passed":20,"failed":0}', 0
        )
        record, failure = self.execute()
        self.assertEqual(record["outcome"], "passed")
        self.assertIsNone(record["failure_code"])
        self.assertIsNone(failure)
        self.assertEqual(
            (self.run_path / "01-distributions.stdout.log").read_bytes(),
            b"result=pass tests=0 passed=20 failed=0\n",
        )
        self.assertEqual(
            (self.run_path / "01-distributions.stderr.log").read_bytes(),
            b"result=pass exit_status=0\n",
        )

    def test_runner_exit_failure_cannot_be_overridden_by_counts(self) -> None:
        self.install_program(
            '{"schema_version":1,"observations":20,"passed":20,"failed":0}', 3
        )
        record, failure = self.execute()
        self.assertEqual(
            record["distribution"], {"observations": 20, "passed": 20, "failed": 0}
        )
        self.assertEqual(record["process_outcome"], "failed")
        self.assertEqual(record["failure_code"], "command_exit_nonzero")
        self.assertEqual(failure, "command_exit_nonzero:distributions")

    def test_zero_exit_with_failed_observation_is_not_success(self) -> None:
        self.install_program(
            '{"schema_version":1,"observations":20,"passed":19,"failed":1}', 0
        )
        record, _failure = self.execute()
        self.assertEqual(record["process_outcome"], "passed")
        self.assertEqual(record["outcome"], "failed")
        self.assertEqual(record["failure_code"], "distribution_requirements_failed")

    def test_invalid_distribution_output_is_explicit(self) -> None:
        self.install_program("not-json", 0)
        record, failure = self.execute()
        self.assertIsNone(record["distribution"])
        self.assertEqual(record["failure_code"], "invalid_distribution_output")
        self.assertEqual(failure, "invalid_distribution_output:distributions")
        self.assertEqual(
            (self.run_path / "01-distributions.stdout.log").read_bytes(),
            b"result=fail tests=0\n",
        )

    def test_failed_to_start_retains_only_closed_failure_summaries(self) -> None:
        record, failure = self.execute(kind="metadata")
        self.assertEqual(record["process_outcome"], "failed_to_start")
        self.assertEqual(record["failure_code"], "command_failed_to_start")
        self.assertEqual(failure, "command_failed_to_start:distributions")
        self.assertEqual(
            (self.run_path / "01-distributions.stdout.log").read_bytes(),
            b"result=fail tests=0\n",
        )
        self.assertEqual(
            (self.run_path / "01-distributions.stderr.log").read_bytes(),
            b"result=fail\n",
        )

    def test_logs_are_private_structured_and_contain_no_process_bytes(self) -> None:
        secret = "sk-" + "C" * 24
        stderr_secret = "ghp_" + "D" * 24
        self.install_program(f"{secret} {ROOT} arbitrary output", 0, stderr_secret)
        record, _failure = self.execute(kind="metadata")
        stdout = self.run_path / record["stdout"]["path"]
        stderr = self.run_path / record["stderr"]["path"]
        self.assertEqual(stdout.read_bytes(), b"result=pass tests=0\n")
        self.assertEqual(stderr.read_bytes(), b"result=pass exit_status=0\n")
        self.assertNotIn(secret.encode(), stdout.read_bytes())
        self.assertNotIn(stderr_secret.encode(), stderr.read_bytes())
        self.assertNotIn(str(ROOT).encode(), stdout.read_bytes())
        self.assertEqual(stdout.stat().st_mode & 0o777, 0o600)

    def test_observed_test_count_is_the_only_retained_process_fact(self) -> None:
        self.install_program(
            "test result: ok. 7 passed; 2 failed; 0 ignored; ordinary output", 0
        )
        record, _failure = self.execute(kind="metadata")
        self.assertEqual(record["test_executions"], 9)
        self.assertEqual(
            (self.run_path / "01-distributions.stdout.log").read_bytes(),
            b"result=pass tests=9\n",
        )


if __name__ == "__main__":
    unittest.main()
