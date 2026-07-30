"""Small Cargo manifest helpers used by release scripts.

The release scripts only need the package version. Python 3.11 provides
``tomllib``; the bounded fallback keeps the scripts usable on the Python 3.9
runtime used by the managed-relay host and local macOS tooling without adding
an unpinned third-party dependency.
"""

from __future__ import annotations

import re
from pathlib import Path


PACKAGE_SECTION = re.compile(r"(?ms)^\[package\]\s*(.*?)(?=^\[|\Z)")
VERSION_FIELD = re.compile(r'(?m)^version\s*=\s*"([^"]+)"\s*(?:#.*)?$')


def package_version(path: Path) -> str:
    source = path.read_text(encoding="utf-8")
    try:
        import tomllib
    except ModuleNotFoundError:
        section = PACKAGE_SECTION.search(source)
        version = VERSION_FIELD.search(section.group(1)) if section else None
        if version is None:
            raise ValueError(f"Cargo package version is missing in {path}")
        return version.group(1)

    try:
        manifest = tomllib.loads(source)
        version = manifest["package"]["version"]
    except (KeyError, TypeError, tomllib.TOMLDecodeError) as error:
        raise ValueError(f"Cargo package version is invalid in {path}") from error
    if not isinstance(version, str) or not version:
        raise ValueError(f"Cargo package version is invalid in {path}")
    return version
