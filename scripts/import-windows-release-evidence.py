#!/usr/bin/env python3
"""Import source-bound Windows release evidence from an immutable Git commit."""

from __future__ import annotations

import argparse
import json
import re
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
COMMIT_PATTERN = re.compile(r"^[0-9a-f]{40}$")
VERSION_PATTERN = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")

EVIDENCE_FILES = {
    "windows-acceptance-record.json": Path("windows") / "windows-acceptance-record.json",
    "windows-resilience-report.json": Path("windows") / "windows-resilience-report.json",
    "managed-relay-verification.json": Path("windows") / "managed-relay-verification.json",
    "managed-diagnostics-audit.json": Path("linux") / "managed-diagnostics-audit.json",
}


def read_git_file(root: Path, ref: str, path: str) -> bytes:
    completed = subprocess.run(
        ["git", "show", f"{ref}:{path}"],
        cwd=root,
        check=False,
        capture_output=True,
    )
    if completed.returncode != 0:
        raise ValueError(f"Evidence file is missing from {ref}: {path}")
    return completed.stdout


def import_evidence(root: Path, ref: str, tag: str, output_root: Path) -> list[Path]:
    ref = ref.strip().lower()
    if COMMIT_PATTERN.fullmatch(ref) is None:
        raise ValueError("--ref must be an immutable 40-character commit SHA")
    if not tag.startswith("v") or VERSION_PATTERN.fullmatch(tag[1:]) is None:
        raise ValueError("--tag must be a version tag such as v0.1.91")

    source_root = f"release-evidence/{tag}"
    imported: list[Path] = []
    for name, relative_destination in EVIDENCE_FILES.items():
        data = read_git_file(root, ref, f"{source_root}/{name}")
        try:
            value = json.loads(data.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            raise ValueError(f"Evidence file is not valid UTF-8 JSON: {name}") from error
        if not isinstance(value, dict):
            raise ValueError(f"Evidence file must contain a JSON object: {name}")
        destination = output_root / relative_destination
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(data)
        imported.append(destination)
    return imported


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ref", required=True, help="Immutable evidence commit SHA")
    parser.add_argument("--tag", required=True, help="Release candidate tag, for example v0.1.91")
    parser.add_argument(
        "--output-root",
        type=Path,
        default=ROOT / "dist",
        help="Destination root containing windows/ and linux/ directories",
    )
    return parser.parse_args()


def main() -> int:
    arguments = parse_args()
    imported = import_evidence(
        ROOT,
        ref=arguments.ref,
        tag=arguments.tag,
        output_root=arguments.output_root,
    )
    for path in imported:
        print(f"Imported release evidence: {path.resolve()}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
