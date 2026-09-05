#!/usr/bin/env python3
"""Run the real diagnostic script against isolated Git/Cargo command doubles."""

import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


REPO_ROOT = Path(__file__).resolve().parents[2]
RUNNER = REPO_ROOT / "scripts" / "remote-battery.sh"
LEGS = ["fmt", "clippy", "tests", "doctests"]
DOUBLE = r'''
import json
import os
from pathlib import Path
import sys

root = Path(os.environ["FIXTURE_ROOT"])
name = Path(sys.argv[0]).name
args = sys.argv[1:]
with (root / "calls.jsonl").open("a", encoding="utf-8") as stream:
    stream.write(json.dumps({"command": name, "args": args,
                             "norn_home": os.environ.get("NORN_HOME")}) + "\n")

if name == "git":
    if args == ["status", "--porcelain", "--untracked-files=no"]:
        if os.environ.get("FIXTURE_STATUS_ERROR") == "1":
            print("fixture Git status failure", file=sys.stderr)
            sys.exit(17)
        if os.environ.get("FIXTURE_DIRTY") == "1":
            print(" M tracked.txt")
    elif args == ["rev-parse", "HEAD"]:
        print("a" * 40)
    else:
        print("unexpected Git command: " + repr(args), file=sys.stderr)
        sys.exit(91)
elif name == "rustc":
    if args != ["--version"]:
        sys.exit(92)
    print("rustc " + os.environ.get("FIXTURE_RUST_VERSION", "1.94.0") + " (fixture)")
elif name == "cargo":
    if args == ["--version"]:
        print("cargo 1.94.0 (fixture)")
        sys.exit(0)
    if args[0] == "fmt":
        if args != ["fmt", "--all", "--", "--check"]:
            print("formatter must be check-only", file=sys.stderr)
            sys.exit(93)
        leg = "fmt"
    elif args[0] == "clippy":
        leg = "clippy"
    elif args[0] == "test":
        leg = "doctests" if "--doc" in args else "tests"
    else:
        sys.exit(94)
    print("fixture raw stdout: " + leg)
    print("fixture raw stderr: " + leg, file=sys.stderr)
    if os.environ.get("FIXTURE_FAIL_LEG") == leg:
        sys.exit(int(os.environ["FIXTURE_FAIL_RC"]))
else:
    sys.exit(95)
'''


class DiagnosticRunnerTests(unittest.TestCase):
    """No fixture can resolve a real Cargo, Rust compiler, or Git executable."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="norn-diagnostic-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "scripts").mkdir()
        (self.root / ".git").mkdir()
        self.runner = self.root / "scripts" / "remote-battery.sh"
        shutil.copyfile(RUNNER, self.runner)
        (self.root / "rust-toolchain.toml").write_text(
            '[toolchain]\nchannel = "1.94.0"\n', encoding="utf-8"
        )
        self.tracked = self.root / "tracked.txt"
        self.tracked.write_text("operator's exact source\n", encoding="utf-8")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        # The runner gets an exclusive PATH. Only harmless host utilities are
        # linked in; all tools that could inspect/mutate a repository or compile
        # are our doubles. No operator environment or credentials are inherited.
        for command in ("dirname", "sed", "mkdir", "hostname", "uname", "date", "cat"):
            executable = shutil.which(command)
            self.assertIsNotNone(executable, f"required fixture utility: {command}")
            (self.bin / command).symlink_to(executable)
        for command in ("git", "rustc", "cargo"):
            double = self.bin / command
            double.write_text(f"#!{sys.executable}\n{DOUBLE}", encoding="utf-8")
            double.chmod(0o755)
        bash = shutil.which("bash")
        self.assertIsNotNone(bash, "bash is required to execute the actual runner")
        self.bash = bash
        self.environment = {
            "PATH": str(self.bin),
            "LC_ALL": "C",
            "FIXTURE_ROOT": str(self.root),
        }

    def run_fixture(self, **values):
        environment = self.environment | values
        return subprocess.run(
            [self.bash, str(self.runner)],
            cwd=self.root,
            env=environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

    def calls(self):
        path = self.root / "calls.jsonl"
        if not path.exists():
            return []
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]

    def leg_calls(self):
        return [
            call for call in self.calls()
            if call["command"] == "cargo" and call["args"] != ["--version"]
        ]

    def assert_all_legs(self, expected_exits):
        logs = self.root / "gate-logs"
        self.assertEqual(len(self.leg_calls()), len(LEGS))
        for leg, expected in zip(LEGS, expected_exits, strict=True):
            self.assertEqual((logs / f"{leg}.exit").read_text(), f"{expected}\n")
            output = (logs / f"{leg}.log").read_text()
            self.assertIn(f"fixture raw stdout: {leg}", output)
            self.assertIn(f"fixture raw stderr: {leg}", output)
        for call in self.leg_calls():
            self.assertEqual(
                call["norn_home"], str(self.root / "target" / "remote-battery-norn-home")
            )

    def test_happy_path_records_logs_and_no_landing_claim(self):
        result = self.run_fixture()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assert_all_legs([0, 0, 0, 0])
        self.assertIn("DIAGNOSTICS GREEN", result.stdout)
        self.assertIn("not a landing receipt", result.stdout)
        summary = (self.root / "gate-logs" / "summary.txt").read_text()
        self.assertIn("repo_battery_205 terminal receipt", summary)
        self.assertIn("git-head:   " + "a" * 40,
                      (self.root / "gate-logs" / "environment.txt").read_text())

    def test_failed_leg_retains_raw_exit_and_later_legs_execute(self):
        result = self.run_fixture(FIXTURE_FAIL_LEG="clippy", FIXTURE_FAIL_RC="23")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assert_all_legs([0, 23, 0, 0])
        self.assertIn("DIAGNOSTICS RED", result.stdout)
        self.assertNotIn("DIAGNOSTICS GREEN", result.stdout)

    def test_formatter_failure_is_red_without_a_source_diff(self):
        before = self.tracked.read_bytes()
        result = self.run_fixture(FIXTURE_FAIL_LEG="fmt", FIXTURE_FAIL_RC="9")
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assert_all_legs([9, 0, 0, 0])
        self.assertEqual(self.tracked.read_bytes(), before)
        git_commands = [call["args"] for call in self.calls() if call["command"] == "git"]
        self.assertEqual(git_commands, [
            ["status", "--porcelain", "--untracked-files=no"], ["rev-parse", "HEAD"]
        ])

    def test_dirty_tracked_tree_is_refused_before_tools_and_preserved(self):
        before = self.tracked.read_bytes()
        result = self.run_fixture(FIXTURE_DIRTY="1")
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertIn("tracked tree is dirty", result.stderr)
        self.assertEqual(self.tracked.read_bytes(), before)
        self.assertEqual(len(self.calls()), 1)
        self.assertFalse((self.root / "gate-logs").exists())

    def test_missing_cargo_is_an_explicit_refusal(self):
        (self.bin / "cargo").unlink()
        result = self.run_fixture()
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertIn("required command unavailable: cargo", result.stderr)
        self.assertEqual(self.leg_calls(), [])
        self.assertNotIn("DIAGNOSTICS GREEN", result.stdout)

    def test_missing_dirname_is_refused_before_root_discovery(self):
        (self.bin / "dirname").unlink()
        result = self.run_fixture()
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertIn("required command unavailable: dirname", result.stderr)
        self.assertEqual(self.calls(), [])
        self.assertFalse((self.root / "gate-logs").exists())
        self.assertNotIn("DIAGNOSTICS GREEN", result.stdout)

    def test_git_status_failure_cannot_pass_cleanliness(self):
        result = self.run_fixture(FIXTURE_STATUS_ERROR="1")
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertIn("cannot establish tracked-tree cleanliness", result.stderr)
        self.assertEqual(self.leg_calls(), [])

    def test_wrong_toolchain_refuses_before_diagnostic_legs(self):
        result = self.run_fixture(FIXTURE_RUST_VERSION="1.94.01")
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        self.assertIn("does not match pinned channel 1.94.0", result.stderr)
        self.assertEqual(self.leg_calls(), [])

    def test_log_creation_failure_cannot_print_green(self):
        (self.root / "gate-logs").write_text("not a directory", encoding="utf-8")
        result = self.run_fixture()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.leg_calls(), [])
        self.assertNotIn("DIAGNOSTICS GREEN", result.stdout)


class VenueDeclarationTests(unittest.TestCase):
    def test_required_legs_are_partitioned_locked_and_not_receipts(self):
        declaration = json.loads((REPO_ROOT / "gates.json").read_text(encoding="utf-8"))
        legs = declaration["legs"]
        self.assertEqual([leg["name"] for leg in legs], LEGS + ["diagnostic-runner"])
        for leg in legs:
            self.assertTrue(leg["cmd"])
            self.assertIn(leg["band"], {"serial", "own_lane", "fanout"})
            if "cargo " in leg["cmd"]:
                self.assertEqual(leg["band"], "serial")
                if leg["name"] != "fmt":
                    self.assertIn("--locked", leg["cmd"])
        by_name = {leg["name"]: leg for leg in legs}
        self.assertIn("--all-targets", by_name["clippy"]["cmd"])
        self.assertIn("-- -D warnings", by_name["clippy"]["cmd"])
        self.assertIn("--all-targets", by_name["tests"]["cmd"])
        self.assertIn("--doc", by_name["doctests"]["cmd"])
        self.assertEqual(by_name["diagnostic-runner"]["band"], "fanout")
        self.assertIn("terminal receipt", declaration["authority_note"])
        self.assertIn("source-binding limitation", declaration["source_binding_note"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
