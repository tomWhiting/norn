"""Repository-local storage boundaries for retained P0 evidence."""

from __future__ import annotations

import subprocess
from dataclasses import dataclass
from pathlib import Path


STORAGE_LAYOUT = "repository_target_v1"


@dataclass(frozen=True)
class RepositoryTargetLayout:
    """The main checkout and its three ignored evidence lanes."""

    worktree: Path
    main_repository: Path
    target_root: Path

    @classmethod
    def locate(cls, worktree: Path) -> "RepositoryTargetLayout":
        resolved_worktree = worktree.resolve()
        result = subprocess.run(
            (
                "git",
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            ),
            cwd=resolved_worktree,
            check=True,
            capture_output=True,
            text=True,
        )
        common_dir = Path(result.stdout.strip()).resolve()
        if common_dir.name != ".git":
            raise RuntimeError("P0 evidence requires a non-bare main repository")
        main_repository = common_dir.parent
        layout = cls(
            worktree=resolved_worktree,
            main_repository=main_repository,
            target_root=main_repository / "target",
        )
        layout.require_real_target_lanes()
        layout.require_ignored_target()
        return layout

    @classmethod
    def discover(cls, worktree: Path) -> "RepositoryTargetLayout":
        layout = cls.locate(worktree)
        layout.require_lane_path(layout.worktree, "worktrees", "evidence worktree")
        return layout

    def require_real_target_lanes(self) -> None:
        for path in (
            self.target_root,
            self.target_root / "worktrees",
            self.target_root / "build",
            self.target_root / "evidence",
        ):
            if path.exists() and path.resolve() != path:
                raise RuntimeError(
                    "repository target evidence lanes must not be symlinks"
                )

    def require_lane_path(self, path: Path, lane: str, label: str) -> Path:
        if lane not in {"worktrees", "build", "evidence"}:
            raise RuntimeError(f"unknown repository target lane: {lane}")
        resolved = path.resolve()
        lane_root = (self.target_root / lane).resolve()
        if resolved == lane_root or lane_root not in resolved.parents:
            raise RuntimeError(
                f"{label} must be inside the repository target/{lane}/ lane"
            )
        return resolved

    def require_ignored_target(self) -> None:
        relative_target = self.target_root.relative_to(self.main_repository)
        result = subprocess.run(
            (
                "git",
                "check-ignore",
                "--quiet",
                "--no-index",
                "--",
                str(relative_target),
            ),
            cwd=self.main_repository,
            check=False,
        )
        if result.returncode != 0:
            raise RuntimeError("the repository target/ tree must be ignored by Git")
        tracked = subprocess.run(
            ("git", "ls-files", "--", str(relative_target)),
            cwd=self.main_repository,
            check=True,
            capture_output=True,
            text=True,
        )
        if tracked.stdout.strip():
            raise RuntimeError(
                "the repository target/ tree must contain no tracked files"
            )


@dataclass(frozen=True)
class ValidatedRunnerPaths:
    layout: RepositoryTargetLayout
    target_dir: Path
    output: Path
    policy_output: Path | None


@dataclass(frozen=True)
class ValidatedAttesterPaths:
    layout: RepositoryTargetLayout
    gate: Path
    distributions: Path
    policy: Path
    output: Path


def validate_runner_paths(
    worktree: Path,
    target_dir: Path,
    output: Path,
    policy_output: Path | None,
) -> ValidatedRunnerPaths:
    layout = RepositoryTargetLayout.discover(worktree)
    build = layout.require_lane_path(target_dir, "build", "Cargo target directory")
    canonical_output = layout.require_lane_path(output, "evidence", "evidence output")
    canonical_policy = None
    artifact_paths = [canonical_output]
    if policy_output is not None:
        canonical_policy = layout.require_lane_path(
            policy_output, "evidence", "policy output"
        )
        artifact_paths.append(canonical_policy)
    require_distinct_files(artifact_paths)
    require_disjoint_lanes(layout.worktree, build, layout.target_root / "evidence")
    return ValidatedRunnerPaths(
        layout=layout,
        target_dir=build,
        output=canonical_output,
        policy_output=canonical_policy,
    )


def validate_attester_paths(
    worktree: Path,
    gate: Path,
    distributions: Path,
    policy: Path,
    output: Path,
) -> ValidatedAttesterPaths:
    layout = RepositoryTargetLayout.discover(worktree)
    artifacts = tuple(
        layout.require_lane_path(path, "evidence", label)
        for path, label in (
            (gate, "gate input"),
            (distributions, "distribution input"),
            (policy, "policy input"),
            (output, "attestation output"),
        )
    )
    require_distinct_files(list(artifacts))
    require_disjoint_lanes(
        layout.worktree,
        layout.target_root / "build",
        layout.target_root / "evidence",
    )
    return ValidatedAttesterPaths(layout, *artifacts)


def require_distinct_files(paths: list[Path]) -> None:
    resolved = [str(path.resolve()).casefold() for path in paths]
    if len(set(resolved)) != len(resolved):
        raise RuntimeError("retained evidence files must be distinct")


def require_disjoint_lanes(*paths: Path) -> None:
    resolved = [path.resolve() for path in paths]
    for index, left in enumerate(resolved):
        for right in resolved[index + 1 :]:
            if left == right or left in right.parents or right in left.parents:
                raise RuntimeError(
                    "evidence worktree, build, and artifact lanes must be disjoint"
                )
