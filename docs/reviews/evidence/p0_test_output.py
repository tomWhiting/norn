"""Parse stable failing-test identities from Rust test output."""

from __future__ import annotations

import re
import shlex
from collections import Counter
from dataclasses import dataclass, replace

from p0_evidence_disclosure import string_has_absolute_path


FAILED_STATUS = re.compile(r"^test (?P<name>.+) \.\.\. FAILED$")
FAILED_RESULT = re.compile(
    r"^test result: FAILED\. \d+ passed; (?P<failed>\d+) failed; \d+ ignored;"
)
TEST_RESULT_COUNTS = re.compile(
    r"test result: .*? (?P<passed>\d+) passed; (?P<failed>\d+) failed; "
    r"(?P<ignored>\d+) ignored;"
)
TEST_STATUS = re.compile(r"^test (?P<name>.+) \.\.\. (?:ok|FAILED|ignored)$")
UNITTEST_COUNT = re.compile(r"^Ran (?P<count>\d+) tests? in \d+(?:\.\d+)?s$")
STABLE_RESULT_LINE = re.compile(
    r"^test result: (?:ok|FAILED)\. \d+ passed; \d+ failed; \d+ ignored; "
    r"\d+ measured; \d+ filtered out; finished in \d+(?:\.\d+)?s$"
)
RUST_TEST_NAME = re.compile(
    r"^(?:r#)?[A-Za-z_][A-Za-z0-9_]*"
    r"(?:::(?:r#)?[A-Za-z_][A-Za-z0-9_]*)*$"
)
DOCTEST_NAME = re.compile(
    r"^(?!/)(?!.*(?:^|/)\.\.(?:/|$))[A-Za-z0-9_.-]+"
    r"(?:/[A-Za-z0-9_.-]+)*\.rs - "
    r"[A-Za-z0-9_:#<>,.&*' ()\[\]-]+ \(line \d+\)"
    r"(?: - compile(?: fail)?)?$"
)
RERUN_HINT = re.compile(
    r"^error: (?:test|doctest) failed, to rerun pass '(?P<command>[^']+)'$"
)
TARGET_VALUE = re.compile(r"^[A-Za-z0-9_.-]+$")


@dataclass(frozen=True)
class TestTargetIdentity:
    package: str
    kind: str
    name: str | None

    def as_record(self) -> dict[str, str | None]:
        return {"package": self.package, "kind": self.kind, "name": self.name}


@dataclass(frozen=True)
class FailureGroup:
    """One test binary's structurally identified failures."""

    names: tuple[str, ...]
    source: str
    declared_failed: int | None
    target: TestTargetIdentity | None

    def as_record(self) -> dict[str, object]:
        return {
            "source": self.source,
            "declared_failed": self.declared_failed,
            "target": self.target.as_record() if self.target is not None else None,
            "names": list(self.names),
        }


@dataclass(frozen=True)
class FailureIdentityReport:
    """Failure names plus whether every failing binary supplied a complete list."""

    groups: tuple[FailureGroup, ...]
    complete: bool

    @property
    def names(self) -> tuple[str, ...]:
        return tuple(name for group in self.groups for name in group.names)


def stable_test_identity(name: str) -> bool:
    """Accept Rust paths and rustdoc's stable, repository-relative test labels."""
    return (
        bool(name)
        and len(name) <= 512
        and (
            RUST_TEST_NAME.fullmatch(name) is not None
            or DOCTEST_NAME.fullmatch(name) is not None
        )
    )


def stable_test_target_record(value: object) -> bool:
    if not isinstance(value, dict) or set(value) != {"package", "kind", "name"}:
        return False
    package = value.get("package")
    kind = value.get("kind")
    name = value.get("name")
    if not isinstance(package, str) or TARGET_VALUE.fullmatch(package) is None:
        return False
    if kind not in {"lib", "doc", "bin", "test", "example", "bench"}:
        return False
    if kind in {"lib", "doc"}:
        return name is None
    return isinstance(name, str) and TARGET_VALUE.fullmatch(name) is not None


def failure_identity_report(stdout: str, stderr: str) -> FailureIdentityReport:
    """Parse final failure lists, falling back to strict status lines if truncated."""
    groups = tuple((*_stream_groups(stdout), *_stream_groups(stderr)))
    targets = _rerun_targets(stdout, stderr)
    summary_indexes = [
        index for index, group in enumerate(groups) if group.source == "summary"
    ]
    mutable_groups = list(groups)
    for index, target in zip(summary_indexes, targets):
        mutable_groups[index] = replace(mutable_groups[index], target=target)
    groups = tuple(mutable_groups)
    bound_targets = [group.target for group in groups if group.target is not None]
    complete = (
        bool(groups)
        and len(targets) == len(summary_indexes)
        and len(set(bound_targets)) == len(bound_targets)
        and all(_group_is_complete(group) for group in groups)
    )
    return FailureIdentityReport(groups=groups, complete=complete)


def failed_test_names(stdout: str, stderr: str) -> list[str]:
    """Compatibility helper returning the report's ordered, non-deduplicated names."""
    return list(failure_identity_report(stdout, stderr).names)


def test_summary_lines(stdout: str, stderr: str) -> list[str]:
    return [
        line.strip()
        for line in (*stdout.splitlines(), *stderr.splitlines())
        if stable_test_summary(line.strip())
    ]


def stable_test_summary(line: str) -> bool:
    return STABLE_RESULT_LINE.fullmatch(line) is not None


def test_counts(summaries: list[str]) -> dict[str, int]:
    counts = {"passed": 0, "failed": 0, "ignored": 0}
    for summary in summaries:
        match = TEST_RESULT_COUNTS.search(summary)
        if match is not None:
            for name in counts:
                counts[name] += int(match.group(name))
    return counts


def test_names(stdout: str, stderr: str) -> list[str]:
    return [
        match.group("name")
        for line in (*stdout.splitlines(), *stderr.splitlines())
        if (match := TEST_STATUS.fullmatch(line.strip())) is not None
        and stable_test_identity(match.group("name"))
    ]


def exact_unittest_count(stdout: str, stderr: str) -> int | None:
    """Return a count only for one successful, unsuffixed unittest result."""
    lines = [line.strip() for line in (*stdout.splitlines(), *stderr.splitlines())]
    matches = [UNITTEST_COUNT.fullmatch(line) for line in lines]
    counts = [int(match.group("count")) for match in matches if match is not None]
    nonempty = [line for line in lines if line]
    if len(counts) != 1 or not nonempty or nonempty[-1] != "OK":
        return None
    return counts[0]


def _stream_groups(output: str) -> tuple[FailureGroup, ...]:
    lines = output.splitlines()
    groups: list[FailureGroup] = []
    summary_names: list[str] = []
    for index, line in enumerate(lines):
        match = FAILED_RESULT.match(line.strip())
        if match is None:
            continue
        names = _adjacent_failure_list(lines, index)
        group = FailureGroup(
            names=names,
            source="summary",
            declared_failed=int(match.group("failed")),
            target=None,
        )
        groups.append(group)
        summary_names.extend(names)

    unmatched = _unmatched_status_names(lines, summary_names)
    for name in unmatched:
        groups.append(
            FailureGroup(
                names=(name,),
                source="status_fallback",
                declared_failed=None,
                target=None,
            )
        )
    return tuple(groups)


def _adjacent_failure_list(lines: list[str], summary_index: int) -> tuple[str, ...]:
    cursor = summary_index - 1
    while cursor >= 0 and not lines[cursor].strip():
        cursor -= 1
    reversed_names: list[str] = []
    while cursor >= 0 and lines[cursor][:1].isspace() and lines[cursor].strip():
        reversed_names.append(lines[cursor].strip())
        cursor -= 1
    if cursor < 0 or lines[cursor].strip() != "failures:":
        return ()
    names = tuple(reversed(reversed_names))
    return tuple(name for name in names if stable_test_identity(name))


def _unmatched_status_names(lines: list[str], summary_names: list[str]) -> list[str]:
    remaining = Counter(summary_names)
    unmatched = []
    for line in lines:
        match = FAILED_STATUS.fullmatch(line.strip())
        if match is None:
            continue
        name = match.group("name")
        if not stable_test_identity(name):
            continue
        if remaining[name] > 0:
            remaining[name] -= 1
        else:
            unmatched.append(name)
    return unmatched


def _group_is_complete(group: FailureGroup) -> bool:
    return (
        group.source == "summary"
        and group.declared_failed is not None
        and group.declared_failed > 0
        and group.target is not None
        and len(group.names) == group.declared_failed
        and len(set(group.names)) == len(group.names)
        and all(stable_test_identity(name) for name in group.names)
    )


def _rerun_targets(stdout: str, stderr: str) -> tuple[TestTargetIdentity | None, ...]:
    targets = []
    for line in (*stdout.splitlines(), *stderr.splitlines()):
        match = RERUN_HINT.fullmatch(line.strip())
        if match is None:
            continue
        targets.append(_parse_rerun_command(match.group("command")))
    return tuple(targets)


def _parse_rerun_command(command: str) -> TestTargetIdentity | None:
    if string_has_absolute_path(command):
        return None
    try:
        tokens = shlex.split(command)
    except ValueError:
        return None
    if len(tokens) not in {3, 4} or tokens[0] != "-p":
        return None
    package = tokens[1]
    selector = tokens[2]
    selectors = {
        "--lib": "lib",
        "--doc": "doc",
        "--bin": "bin",
        "--test": "test",
        "--example": "example",
        "--bench": "bench",
    }
    kind = selectors.get(selector)
    if kind is None or TARGET_VALUE.fullmatch(package) is None:
        return None
    needs_name = kind not in {"lib", "doc"}
    if needs_name != (len(tokens) == 4):
        return None
    name = tokens[3] if needs_name else None
    if name is not None and TARGET_VALUE.fullmatch(name) is None:
        return None
    return TestTargetIdentity(package=package, kind=kind, name=name)
