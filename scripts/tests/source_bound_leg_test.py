#!/usr/bin/env python3
"""Exercise the source guard in committed temporary repositories, without Cargo."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SOURCE = Path(__file__).resolve().parents[1] / "source-bound-leg.py"


class SourceBoundLegTest(unittest.TestCase):
    """Test actual guard execution and Git witnesses, including failure paths."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="norn-source-guard-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.git("init", "-q")
        self.git("config", "user.name", "Source guard fixture")
        self.git("config", "user.email", "source-guard@example.invalid")
        self.git("config", "commit.gpgSign", "false")
        (self.root / "scripts").mkdir()
        shutil.copyfile(SOURCE, self.root / "scripts/source-bound-leg.py")
        (self.root / "gates.json").write_text('{"legs": []}\n')
        (self.root / "tracked.txt").write_text("original\n")
        (self.root / ".gitignore").write_text("/target/\n")
        self.git("add", ".")
        self.git("commit", "-qm", "Committed fixture")

    def git(self, *arguments):
        return subprocess.run(["git", *arguments], cwd=self.root, capture_output=True,
                              text=True, check=True).stdout.strip()

    def run_guard(self, code, environment=None):
        result = subprocess.run(
            [sys.executable, "scripts/source-bound-leg.py", "--", sys.executable, "-c", code],
            cwd=self.root, capture_output=True, text=True, check=False, env=environment,
        )
        records = [json.loads(line) for line in result.stdout.splitlines()]
        return result, records

    def assert_refused_before_command(self):
        result, records = self.run_guard("from pathlib import Path; Path('executed').touch()")
        self.assertEqual(result.returncode, 126, result.stderr)
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]["verdict"], "refused")
        self.assertFalse((self.root / "executed").exists())
        return records[0]

    def test_clean_command_records_exact_head_and_digests(self):
        result, records = self.run_guard("pass")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(records[0]["witness"], records[1]["witness"])
        self.assertEqual(records[1]["verdict"], "green")
        witness = records[1]["witness"]
        self.assertEqual(witness["head"], self.git("rev-parse", "HEAD"))
        self.assertEqual(witness["dirty"], [])
        self.assertEqual(witness["guard_sha256"], hashlib.sha256(SOURCE.read_bytes()).hexdigest())
        self.assertEqual(witness["declaration_sha256"], hashlib.sha256(
            (self.root / "gates.json").read_bytes()).hexdigest())

    def test_tracked_dirty_refuses_without_execution_or_cleanup(self):
        (self.root / "tracked.txt").write_text("changed\n")
        self.assert_refused_before_command()
        self.assertEqual((self.root / "tracked.txt").read_text(), "changed\n")

    def test_staged_dirty_refuses(self):
        (self.root / "tracked.txt").write_text("staged\n")
        self.git("add", "tracked.txt")
        self.assert_refused_before_command()

    def test_same_length_restored_mtime_cannot_hide_blob_change(self):
        tracked = self.root / "tracked.txt"
        os.utime(tracked, ns=(0, 0))
        self.git("config", "core.trustctime", "false")
        self.git("config", "core.checkStat", "minimal")
        self.git("update-index", "--refresh")
        self.assertEqual(self.git("status", "--porcelain"), "")
        tracked.write_text("mutated!\n")
        os.utime(tracked, ns=(0, 0))
        self.assertEqual(self.git("status", "--porcelain"), "")
        record = self.assert_refused_before_command()
        self.assertIn("blob mismatch: tracked.txt", record["witness"]["dirty"])

    def test_executable_mode_change_refuses_with_filemode_disabled(self):
        self.git("config", "core.fileMode", "false")
        (self.root / "tracked.txt").chmod(0o755)
        self.assertEqual(self.git("status", "--porcelain"), "")
        record = self.assert_refused_before_command()
        self.assertIn("executable mode mismatch: tracked.txt", record["witness"]["dirty"])

    def test_symlink_target_bytes_are_compared(self):
        link = self.root / "link"
        link.symlink_to("tracked.txt")
        self.git("add", "link")
        self.git("commit", "-qm", "Add symlink")
        clean, records = self.run_guard("pass")
        self.assertEqual(clean.returncode, 0, records)
        link.unlink()
        link.symlink_to("gates.json")
        record = self.assert_refused_before_command()
        self.assertIn("blob mismatch: link", record["witness"]["dirty"])

    def test_untracked_source_refuses(self):
        (self.root / "build.rs").write_text("untracked source\n")
        self.assert_refused_before_command()

    def test_local_excludes_do_not_hide_untracked_source(self):
        (self.root / ".git/info/exclude").write_text("build.rs\n")
        (self.root / "build.rs").write_text("untracked source\n")
        self.assert_refused_before_command()

    def test_gitignored_build_outputs_are_allowed(self):
        (self.root / "target").mkdir()
        (self.root / "target/existing").touch()
        result, records = self.run_guard("from pathlib import Path; Path('target/new').touch()")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(records[1]["witness"]["dirty"], [])

    def test_post_command_dirty_is_red(self):
        result, records = self.run_guard("from pathlib import Path; Path('tracked.txt').write_text('changed')")
        self.assertEqual(result.returncode, 126, result.stderr)
        self.assertEqual(records[1]["command_exit"], 0)
        self.assertTrue(records[1]["witness"]["dirty"])

    def test_moved_head_is_red_even_when_tree_is_clean(self):
        result, records = self.run_guard(
            "import subprocess; subprocess.run(['git','commit','--allow-empty','-qm','Moved HEAD'], check=True)"
        )
        self.assertEqual(result.returncode, 126, result.stderr)
        self.assertNotEqual(records[0]["witness"]["head"], records[1]["witness"]["head"])
        self.assertEqual(records[1]["witness"]["dirty"], [])

    def test_command_failure_is_preserved(self):
        result, records = self.run_guard("import sys; sys.exit(23)")
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertEqual(records[1]["command_exit"], 23)

    def test_command_failure_survives_dirty_postcheck(self):
        result, records = self.run_guard(
            "from pathlib import Path; import sys; Path('tracked.txt').write_text('changed'); sys.exit(23)"
        )
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertIsNotNone(records[1]["source_error"])

    def test_git_failure_refuses_before_execution(self):
        environment = dict(os.environ, GIT_DIR=str(self.root / "absent-git-directory"))
        result, records = self.run_guard("raise SystemExit(99)", environment)
        self.assertEqual(result.returncode, 126, result.stderr)
        self.assertEqual(len(records), 1)
        self.assertIn("git", records[0]["error"])

    def test_git_failure_after_command_is_red(self):
        result, records = self.run_guard("from pathlib import Path; Path('.git').rename('displaced-git')")
        self.assertEqual(result.returncode, 126, result.stderr)
        self.assertIsNone(records[1]["witness"])
        self.assertIn("git", records[1]["source_error"])

    def test_assume_unchanged_cannot_hide_dirty_source(self):
        self.git("update-index", "--assume-unchanged", "tracked.txt")
        (self.root / "tracked.txt").write_text("hidden change\n")
        record = self.assert_refused_before_command()
        self.assertIn("assume-unchanged", record["error"])

    def test_skip_worktree_cannot_hide_dirty_source(self):
        self.git("update-index", "--skip-worktree", "tracked.txt")
        (self.root / "tracked.txt").write_text("hidden change\n")
        record = self.assert_refused_before_command()
        self.assertIn("skip-worktree", record["error"])

    def test_edit_then_restore_is_explicit_pre_post_limit(self):
        result, records = self.run_guard(
            "from pathlib import Path; p=Path('tracked.txt'); original=p.read_bytes(); "
            "p.write_text('changed during command'); p.write_bytes(original)"
        )
        # A pre/post witness cannot establish immutability during the command.
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(records[0]["witness"], records[1]["witness"])

    def test_declared_legs_all_use_guard_and_both_python_suites(self):
        declaration = json.loads((SOURCE.parent.parent / "gates.json").read_text())
        self.assertEqual({leg["name"] for leg in declaration["legs"]},
                         {"fmt", "clippy", "live-smoke-build",
                          "tests", "doctests", "diagnostic-runner"})
        for leg in declaration["legs"]:
            self.assertTrue(leg["cmd"].startswith("python3 scripts/source-bound-leg.py -- "), leg)
        runner = next(leg for leg in declaration["legs"] if leg["name"] == "diagnostic-runner")
        self.assertIn("remote_battery_test.py && python3 scripts/tests/source_bound_leg_test.py", runner["cmd"])


if __name__ == "__main__":
    unittest.main()
