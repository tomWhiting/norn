"""Conservative repository-wide artifact-writer candidate inventory."""

import re
from pathlib import Path
from typing import Callable

import p0_module_references as module_references


ByteRange = tuple[int, int]
TestOnlyRanges = Callable[[Path, Path, Path], tuple[list[ByteRange], list[Path]]]

ARTIFACT_WRITER = re.compile(
    r"(?:PrivateRoot::(?:create|open)|"
    r"\.(?:create_new|open_append_create|open_lock|rename|publish_new|"
    r"create_dir_all|remove_file|remove_dir_all|set_len)\s*\(|"
    r"(?:std::|tokio::)?fs::(?:write|rename|remove_file|remove_dir_all|"
    r"create_dir|create_dir_all|copy|hard_link|symlink)\s*\(|"
    r"(?:(?:std|tokio)::fs::)?File::create\s*\(|"
    r"(?:(?:std|tokio)::fs::)?OpenOptions::new\s*\()"
)


def candidates(
    repo: Path,
    rule: Path,
    test_only_files: set[module_references.FileIdentity],
    test_only_ranges: TestOnlyRanges,
) -> list[dict]:
    records: list[dict] = []
    for source in sorted(repo.glob("crates/**/*.rs")):
        relative = source.relative_to(repo)
        path_parts = relative.parts
        integration_test = (
            len(path_parts) >= 3
            and path_parts[0] == "crates"
            and "tests" in path_parts[2:]
        )
        if (
            module_references.file_identity(source) in test_only_files
            or integration_test
        ):
            continue
        ranges, _modules = test_only_ranges(repo, rule, source)
        data = source.read_bytes()
        offset = 0
        for line_number, raw_line in enumerate(data.splitlines(keepends=True), start=1):
            in_test_item = any(start <= offset < end for start, end in ranges)
            line = raw_line.decode("utf-8")
            if not in_test_item and ARTIFACT_WRITER.search(line):
                records.append(
                    {
                        "file": str(relative),
                        "line": line_number,
                        "text": line.strip(),
                    }
                )
            offset += len(raw_line)
    return records
