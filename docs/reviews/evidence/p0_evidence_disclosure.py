"""Disclosure checks shared by P0 evidence generation and attestation."""

from __future__ import annotations

import re
from typing import Final


FILE_URI: Final = re.compile(r"file://", re.IGNORECASE)
WINDOWS_DRIVE_PATH: Final = re.compile(r"(?<![A-Za-z0-9])(?:[A-Za-z]:[\\/])")
WINDOWS_UNC_PATH: Final = re.compile(
    r"(?<![A-Za-z0-9_:/\\])(?:\\{2,}|//)[^\\/\s]+[\\/]"
)
UNIX_ABSOLUTE_PATH: Final = re.compile(
    r"(?<![A-Za-z0-9_.~+/-])/(?!/)(?:[^\s)'\";,]+/)*[^\s)'\";,]*"
)


def string_has_absolute_path(value: str) -> bool:
    """Reject standalone or embedded Unix, drive, UNC, and file-URI paths."""
    return any(
        pattern.search(value) is not None
        for pattern in (
            FILE_URI,
            WINDOWS_DRIVE_PATH,
            WINDOWS_UNC_PATH,
            UNIX_ABSOLUTE_PATH,
        )
    )


def contains_absolute_path(value: object) -> bool:
    if isinstance(value, dict):
        return any(contains_absolute_path(child) for child in value.values())
    if isinstance(value, list):
        return any(contains_absolute_path(child) for child in value)
    return isinstance(value, str) and string_has_absolute_path(value)
