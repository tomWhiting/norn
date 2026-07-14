"""Tests for the repository-local P0 evidence path boundary."""

from __future__ import annotations

import unittest
from pathlib import Path
from unittest.mock import patch

from p0_evidence_paths import (
    RepositoryTargetLayout,
    require_disjoint_lanes,
    require_distinct_files,
    validate_runner_paths,
)


class EvidencePathTests(unittest.TestCase):
    def setUp(self) -> None:
        self.layout = RepositoryTargetLayout(
            worktree=Path("/repo/target/worktrees/run"),
            main_repository=Path("/repo"),
            target_root=Path("/repo/target"),
        )

    def test_each_storage_class_has_a_separate_target_lane(self) -> None:
        self.assertEqual(
            self.layout.require_lane_path(
                Path("/repo/target/worktrees/run"), "worktrees", "worktree"
            ),
            Path("/repo/target/worktrees/run"),
        )
        self.assertEqual(
            self.layout.require_lane_path(
                Path("/repo/target/build/run"), "build", "build"
            ),
            Path("/repo/target/build/run"),
        )
        self.assertEqual(
            self.layout.require_lane_path(
                Path("/repo/target/evidence/run/gate.json"), "evidence", "artifact"
            ),
            Path("/repo/target/evidence/run/gate.json"),
        )

    def test_external_temporary_path_is_rejected(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "repository target/build"):
            self.layout.require_lane_path(Path("/tmp/build"), "build", "build")

    def test_main_worktree_is_rejected(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "repository target/worktrees"):
            self.layout.require_lane_path(Path("/repo"), "worktrees", "worktree")

    def test_lane_root_itself_is_not_an_evidence_path(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "repository target/evidence"):
            self.layout.require_lane_path(
                Path("/repo/target/evidence"), "evidence", "artifact"
            )

    def test_artifact_aliases_are_rejected(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "must be distinct"):
            require_distinct_files([Path("/repo/target/evidence/gate.json")] * 2)

        with self.assertRaisesRegex(RuntimeError, "must be distinct"):
            require_distinct_files(
                [
                    Path("/repo/target/evidence/gate.json"),
                    Path("/repo/target/evidence/GATE.json"),
                ]
            )

    def test_overlapping_storage_lanes_are_rejected(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "must be disjoint"):
            require_disjoint_lanes(
                Path("/repo/target/build"), Path("/repo/target/build/nested")
            )

    def test_runner_returns_the_canonical_paths_it_validated(self) -> None:
        with patch.object(RepositoryTargetLayout, "discover", return_value=self.layout):
            validated = validate_runner_paths(
                self.layout.worktree,
                Path("/repo/target/build/run"),
                Path("/repo/target/evidence/run/gate.json"),
                Path("/repo/target/evidence/run/policy.json"),
            )

        self.assertEqual(validated.target_dir, Path("/repo/target/build/run"))
        self.assertEqual(validated.output, Path("/repo/target/evidence/run/gate.json"))
        self.assertEqual(
            validated.policy_output, Path("/repo/target/evidence/run/policy.json")
        )

    def test_runner_rejects_an_external_output(self) -> None:
        with (
            patch.object(RepositoryTargetLayout, "discover", return_value=self.layout),
            self.assertRaisesRegex(RuntimeError, "repository target/evidence"),
        ):
            validate_runner_paths(
                self.layout.worktree,
                Path("/repo/target/build/run"),
                Path("/tmp/gate.json"),
                None,
            )


if __name__ == "__main__":
    unittest.main()
