#!/usr/bin/env python3
"""Create a source-bound Windows candidate bundle.

The Windows installer is intentionally kept as the primary artifact.  This
helper packages that installer together with the exact verification reports so
that a copied bundle cannot silently mix artifacts from different commits.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import zipfile
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
EXPECTED_SCOPE = {
    "target": "windows-10/11-x64",
    "macos_release": False,
    "mobile_release": False,
}
SOURCE_COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")


class CandidatePackageError(ValueError):
    """Raised when candidate artifacts are missing or bound inconsistently."""


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise CandidatePackageError(f"cannot read JSON report {path}: {error}") from error
    if not isinstance(value, dict):
        raise CandidatePackageError(f"JSON report {path} must contain an object")
    return value


def _require_source_binding(report: dict[str, Any], *, name: str, commit: str) -> None:
    if report.get("source_commit") != commit:
        raise CandidatePackageError(f"{name} does not match candidate source commit")
    if report.get("source_dirty") is not False:
        raise CandidatePackageError(f"{name} is not bound to a clean source checkout")


def validate_candidate_artifacts(root: Path, dist: Path) -> tuple[str, str, list[Path]]:
    """Validate reports and return version, source commit and ordered files."""

    manifest_path = dist / "windows-installer-manifest.json"
    manifest = read_json(manifest_path)
    version = manifest.get("version")
    source_commit = manifest.get("source_commit")
    if not isinstance(version, str) or not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise CandidatePackageError("installer manifest has an invalid version")
    if not isinstance(source_commit, str) or not SOURCE_COMMIT_RE.fullmatch(source_commit):
        raise CandidatePackageError("installer manifest has an invalid source commit")
    if manifest.get("schema") != 1 or manifest.get("passed") is not True:
        raise CandidatePackageError("installer manifest did not pass validation")
    if manifest.get("source_dirty") is not False:
        raise CandidatePackageError("installer manifest is bound to a dirty checkout")
    if manifest.get("release_scope") != EXPECTED_SCOPE:
        raise CandidatePackageError("installer manifest has an unexpected release scope")

    installer = manifest.get("installer")
    if not isinstance(installer, dict):
        raise CandidatePackageError("installer manifest has no installer metadata")
    installer_name = installer.get("file_name")
    if not isinstance(installer_name, str) or Path(installer_name).name != installer_name:
        raise CandidatePackageError("installer manifest has an invalid installer filename")
    installer_path = dist / installer_name
    if not installer_path.is_file():
        raise CandidatePackageError(f"installer is missing: {installer_path}")
    if installer.get("size_bytes") != installer_path.stat().st_size:
        raise CandidatePackageError("installer size does not match its manifest")
    if installer.get("sha256") != sha256(installer_path):
        raise CandidatePackageError("installer hash does not match its manifest")

    report_names = [
        "windows-release-verification.json",
        "windows-resilience-report.json",
        "windows-acceptance-record.json",
        "windows-release-readiness.json",
    ]
    reports: dict[str, dict[str, Any]] = {}
    for name in report_names:
        path = dist / name
        if not path.is_file():
            raise CandidatePackageError(f"required report is missing: {path}")
        reports[name] = read_json(path)

    _require_source_binding(reports["windows-release-verification.json"], name="release verification", commit=source_commit)
    _require_source_binding(reports["windows-resilience-report.json"], name="resilience report", commit=source_commit)
    _require_source_binding(reports["windows-acceptance-record.json"], name="acceptance record", commit=source_commit)
    _require_source_binding(reports["windows-release-readiness.json"], name="readiness report", commit=source_commit)
    if reports["windows-release-verification.json"].get("version") != version:
        raise CandidatePackageError("release verification version does not match the installer")
    if reports["windows-acceptance-record.json"].get("installer", {}).get("sha256") != installer.get("sha256"):
        raise CandidatePackageError("acceptance record does not reference this installer")

    optional = [
        dist / "managed-relay-verification.json",
        dist.parent / "linux" / "managed-relay-host-audit.json",
        dist.parent / "linux" / "managed-diagnostics-audit.json",
    ]
    files = [installer_path, *(dist / name for name in report_names)]
    files.extend(path for path in optional if path.is_file())
    return version, source_commit, files


def git_output(root: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args], cwd=root, capture_output=True, text=True, check=True
    )
    return result.stdout.strip()


def package_candidate(root: Path = ROOT, output: Path | None = None) -> Path:
    dist = root / "dist" / "windows"
    version, source_commit, files = validate_candidate_artifacts(root, dist)
    if git_output(root, "rev-parse", "HEAD") != source_commit:
        raise CandidatePackageError("candidate reports do not match current HEAD")
    if git_output(root, "status", "--porcelain"):
        raise CandidatePackageError("candidate packaging requires a clean checkout")

    destination = output or dist / f"DeskLink-Windows-candidate-{version}-{source_commit[:12]}.zip"
    destination = destination.resolve()
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination in {path.resolve() for path in files}:
        raise CandidatePackageError("candidate ZIP cannot replace an input artifact")
    with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for path in files:
            archive.write(path, arcname=path.name)
    return destination


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Package a source-bound Windows candidate bundle.")
    parser.add_argument("--output", type=Path, help="optional output ZIP path")
    return parser.parse_args()


def main() -> int:
    arguments = parse_args()
    try:
        path = package_candidate(output=arguments.output)
    except CandidatePackageError as error:
        raise SystemExit(f"Windows candidate package failed: {error}") from error
    print(f"Candidate bundle: {path}")
    print(f"SHA-256: {sha256(path)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
