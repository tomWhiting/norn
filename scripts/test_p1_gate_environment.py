"""Isolation tests for the P1 gate child environment."""

import importlib.util
import json
import os
import shutil
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent
MODULE_PATH = ROOT / "scripts/p1_gate_environment.py"
SPEC = importlib.util.spec_from_file_location("p1_gate_environment", MODULE_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("cannot load P1 gate environment module")
ENVIRONMENT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ENVIRONMENT)


class GateEnvironmentTests(unittest.TestCase):
    def setUp(self) -> None:
        self.test_path = (
            ROOT / "target/p1-gate-tests" / f"environment-{os.getpid()}-{id(self)}"
        )
        self.repo = self.test_path / "repo"
        self.home = self.test_path / "account-home"
        self.repo.mkdir(parents=True)
        self.home.mkdir()
        self.tools = {
            "home": self.home,
            "toolchain_bin": Path("/closed/rust-toolchain/bin"),
            "paths": {
                "rustc": Path("/closed/rustc"),
                "rustdoc": Path("/closed/rustdoc"),
            },
        }

    def tearDown(self) -> None:
        shutil.rmtree(self.test_path)

    def test_child_environment_is_an_allowlist_without_credentials_or_agent(
        self,
    ) -> None:
        with mock.patch.dict(
            os.environ,
            {
                "OPENAI_API_KEY": "sensitive-parent-value",
                "SSH_AUTH_SOCK": "/agent/socket",
                "PATH": "/caller/shims",
                "CARGO_HOME": "/caller/cargo-home",
            },
            clear=True,
        ):
            actual, recorded = ENVIRONMENT.controlled_environment(
                self.repo, "run-one", self.tools, None, None
            )
        self.assertNotIn("OPENAI_API_KEY", actual)
        self.assertNotIn("SSH_AUTH_SOCK", actual)
        self.assertNotIn("/caller/shims", actual["PATH"])
        self.assertNotIn("/caller/cargo-home", actual["CARGO_HOME"])
        self.assertEqual(recorded["caller_environment_inherited"], [])
        self.assertEqual(recorded["credential_environment_inherited"], [])
        self.assertNotIn(str(self.test_path), json.dumps(recorded, sort_keys=True))

    def test_private_runtime_directories_have_no_group_or_other_access(self) -> None:
        actual, _recorded = ENVIRONMENT.controlled_environment(
            self.repo, "run-two", self.tools, None, None
        )
        for name in ("HOME", "TMPDIR", "CARGO_HOME", "CARGO_TARGET_DIR"):
            mode = Path(actual[name]).stat().st_mode & 0o777
            self.assertEqual(mode, 0o700)

    def test_only_dependency_cache_directories_are_bridged(self) -> None:
        (self.home / ".cargo/registry").mkdir(parents=True)
        cargo_home, bridges = ENVIRONMENT.prepare_cache_bridges(self.repo, self.home)
        self.assertEqual(bridges, ["cargo-registry-cache"])
        self.assertTrue((cargo_home / "registry").is_symlink())
        self.assertFalse((cargo_home / "credentials.toml").exists())
        self.assertFalse((cargo_home / "config.toml").exists())

    def test_cargo_credentials_in_isolated_home_are_rejected(self) -> None:
        cargo_home = self.repo / "target/p1-gate/cargo-home"
        ENVIRONMENT.ensure_private_directory(self.repo, cargo_home)
        (cargo_home / "credentials.toml").write_text("private", encoding="utf-8")
        with self.assertRaises(ENVIRONMENT.EnvironmentError):
            ENVIRONMENT.prepare_cache_bridges(self.repo, self.home)

    def test_cache_bridge_mutation_is_rejected_at_finalization(self) -> None:
        (self.home / ".cargo/registry").mkdir(parents=True)
        cargo_home, bridges = ENVIRONMENT.prepare_cache_bridges(self.repo, self.home)
        (cargo_home / "registry").unlink()
        replacement = self.test_path / "replacement"
        replacement.mkdir()
        (cargo_home / "registry").symlink_to(replacement, target_is_directory=True)
        with self.assertRaises(ENVIRONMENT.EnvironmentError):
            ENVIRONMENT.verify_cargo_isolation(cargo_home, self.home, bridges)

    def test_group_writable_tool_is_rejected(self) -> None:
        tool = self.test_path / "tool"
        tool.write_bytes(b"tool")
        tool.chmod(0o775)
        with self.assertRaises(ENVIRONMENT.EnvironmentError):
            ENVIRONMENT.regular_tool(tool, "test-tool")


if __name__ == "__main__":
    unittest.main()
