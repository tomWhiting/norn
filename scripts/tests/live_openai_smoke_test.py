#!/usr/bin/env python3
"""Exercise the real live-lane runner/guard without Cargo, credentials or network."""

import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
RUNNER = ROOT / "scripts/live-openai-smoke.py"
FAKE_CARGO = r'''
import json, os, sys
from pathlib import Path
Path(os.environ['CALL_RECORD']).write_text(json.dumps({
    'args': sys.argv[1:],
    'credential_present': bool(os.environ.get('OPENAI_TEST_KEY')),
    'jobs': os.environ.get('CARGO_BUILD_JOBS'),
}))
# Deliberate synthetic secret echo: the receipt must never forward raw output.
print(os.environ['OPENAI_TEST_KEY'])
print(os.environ['OPENAI_TEST_KEY'], file=sys.stderr)
mode = os.environ.get('FIXTURE_MODE', 'pass')
if mode == 'fail':
    sys.exit(9)
if mode == 'prerequisite':
    print('NORN_TEST_PREREQUISITE_UNMET: controlled fixture')
if mode != 'zero-tests':
    print('test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s')
'''


class LiveSmokeRunnerTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="norn-live-smoke-fixture-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.git("init", "-q")
        self.git("config", "user.name", "Live smoke fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("config", "commit.gpgSign", "false")
        (self.repo / "scripts").mkdir()
        for name in ("live-openai-smoke.py", "source-bound-leg.py"):
            shutil.copyfile(ROOT / "scripts" / name, self.repo / "scripts" / name)
        (self.repo / "gates.json").write_text('{"legs": []}\n')
        (self.repo / "tracked.txt").write_text("source\n")
        self.git("add", ".")
        self.git("commit", "-qm", "Committed fixture")
        self.head = self.git("rev-parse", "HEAD")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        cargo = self.bin / "cargo"
        cargo.write_text(f"#!{sys.executable}\n" + FAKE_CARGO)
        cargo.chmod(0o755)
        self.call_record = self.root / "calls.json"
        self.secret = "synthetic-fixture-only-not-a-real-credential"

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, capture_output=True,
                              text=True, check=True).stdout.strip()

    def run_lane(self, **overrides):
        environment = dict(os.environ, OPENAI_TEST_KEY=self.secret,
                           REPO_BATTERY_LEG_JOBS="2", CALL_RECORD=str(self.call_record),
                           PATH=str(self.bin) + os.pathsep + os.environ["PATH"])
        environment.update(overrides)
        result = subprocess.run([sys.executable, str(self.repo / "scripts/live-openai-smoke.py"),
                                 str(self.repo), self.head], env=environment,
                                capture_output=True, text=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn(self.secret, result.stdout + result.stderr)
        self.assertEqual(result.stderr, "")
        return json.loads(result.stdout)

    def test_missing_key_refuses_before_command(self):
        receipt = self.run_lane(OPENAI_TEST_KEY="")
        self.assertEqual(receipt["verdict"], "refused")
        self.assertIn("OPENAI_TEST_KEY", receipt["reason"])
        self.assertIn("NORN_TEST_PREREQUISITE_UNMET", receipt["reason"])
        self.assertFalse(self.call_record.exists())

    def test_missing_cap_refuses_before_command(self):
        receipt = self.run_lane(REPO_BATTERY_LEG_JOBS="")
        self.assertEqual(receipt["verdict"], "refused")
        self.assertIn("REPO_BATTERY_LEG_JOBS", receipt["reason"])
        self.assertFalse(self.call_record.exists())

    def test_wrong_commit_refuses_before_command(self):
        self.head = "0" * 40
        receipt = self.run_lane()
        self.assertEqual(receipt["verdict"], "refused")
        self.assertIn("HEAD", receipt["reason"])
        self.assertFalse(self.call_record.exists())

    def test_success_requires_guarded_exact_target_and_hides_raw_output(self):
        receipt = self.run_lane()
        self.assertEqual(receipt["verdict"], "green", receipt)
        self.assertEqual(receipt["head_commit"], self.head)
        self.assertEqual(receipt["command_exit"], 0)
        call = json.loads(self.call_record.read_text())
        self.assertEqual(call["args"], ["test", "--locked", "-p", "norn", "--features",
                                       "live-api-smoke", "--test", "live_openai_smoke", "--",
                                       "--exact", "openai_live_hello_smoke", "--nocapture"])
        self.assertTrue(call["credential_present"])
        self.assertEqual(call["jobs"], "2")

    def test_failed_command_remains_red_without_raw_output(self):
        receipt = self.run_lane(FIXTURE_MODE="fail")
        self.assertEqual(receipt["verdict"], "red")
        self.assertEqual(receipt["command_exit"], 9)

    def test_zero_executed_tests_cannot_pass(self):
        receipt = self.run_lane(FIXTURE_MODE="zero-tests")
        self.assertEqual(receipt["verdict"], "refused")
        self.assertIn("exactly one", receipt["reason"])

    def test_announced_prerequisite_cannot_pass_even_with_zero_exit(self):
        receipt = self.run_lane(FIXTURE_MODE="prerequisite")
        self.assertEqual(receipt["verdict"], "refused")
        self.assertIn("prerequisite", receipt["reason"])

    def test_dirty_source_cannot_run_live_command(self):
        (self.repo / "tracked.txt").write_text("dirty source\n")
        receipt = self.run_lane()
        self.assertEqual(receipt["verdict"], "red")
        self.assertEqual(receipt["command_exit"], 126)
        self.assertFalse(self.call_record.exists())

    def test_absent_source_witness_cannot_pass(self):
        (self.repo / "scripts/source-bound-leg.py").write_text("print('no witness')\n")
        self.git("add", ".")
        self.git("commit", "-qm", "Guard fixture missing evidence")
        self.head = self.git("rev-parse", "HEAD")
        receipt = self.run_lane()
        self.assertEqual(receipt["verdict"], "refused")
        self.assertIn("source guard witnesses", receipt["reason"])

    def test_parent_environment_carriage_reads_only_requested_name(self):
        isolated_module = self.root / "imported-live-smoke.py"
        shutil.copyfile(RUNNER, isolated_module)
        spec = importlib.util.spec_from_file_location("live_smoke_fixture_module", isolated_module)
        self.assertIsNotNone(spec)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        parent = self.root / "parent-environment"
        parent.write_bytes(b"UNRELATED=not-selected\0OPENAI_TEST_KEY=synthetic-parent-only\0")
        self.assertEqual(module.worker_value("OPENAI_TEST_KEY", {}, parent), "synthetic-parent-only")
        self.assertIsNone(module.worker_value("REPO_BATTERY_LEG_JOBS", {}, parent))
        self.assertEqual(module.worker_value("OPENAI_TEST_KEY", {"OPENAI_TEST_KEY": ""}, parent), "")


if __name__ == "__main__":
    unittest.main()
