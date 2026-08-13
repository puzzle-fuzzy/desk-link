#!/usr/bin/env python3
"""Create a candidate-bound Windows manual acceptance record template."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
COMMIT_PATTERN = re.compile(r"^[0-9a-f]{40}$")
VERSION_PATTERN = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")
MANUAL_CHECK_IDS = (
    "two_windows_acceptance",
    "long_soak_acceptance",
    "smartscreen_acceptance",
)
MANUAL_ACCEPTANCE_SCHEMA = 2
EXPECTED_SCOPE = {
    "target": "windows-10/11-x64",
    "macos_release": False,
    "mobile_release": False,
}


def git_head(root: Path) -> str:
    completed = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=root,
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    value = completed.stdout.strip().lower()
    if completed.returncode != 0 or COMMIT_PATTERN.fullmatch(value) is None:
        raise SystemExit("Could not read the current Git commit SHA")
    return value


def package_version(root: Path) -> str:
    source = (root / "tools" / "windows-installer" / "Cargo.toml").read_text(
        encoding="utf-8"
    )
    match = re.search(r"(?m)^version\s*=\s*\"([^\"]+)\"", source)
    version = match.group(1) if match else ""
    if VERSION_PATTERN.fullmatch(version) is None:
        raise SystemExit("Windows installer package version is invalid")
    return version


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--operator", required=True, help="Name or team recording the acceptance")
    parser.add_argument(
        "--output",
        type=Path,
        default=ROOT / "dist" / "windows" / "windows-acceptance-record.json",
    )
    return parser.parse_args()


def validate_candidate_manifest(
    manifest: dict[str, object],
    *,
    version: str,
    source_commit: str,
    installer_name: str,
    installer_path: Path,
) -> str:
    if (
        manifest.get("schema") != 1
        or manifest.get("passed") is not True
        or manifest.get("version") != version
        or manifest.get("source_commit") != source_commit
        or manifest.get("source_dirty") is not False
        or manifest.get("release_scope") != EXPECTED_SCOPE
    ):
        raise ValueError("Installer manifest is stale or does not match the current Windows candidate")
    installer = manifest.get("installer")
    expected_sha256 = installer.get("sha256") if isinstance(installer, dict) else None
    expected_size = installer.get("size_bytes") if isinstance(installer, dict) else None
    if (
        not isinstance(installer, dict)
        or installer.get("file_name") != installer_name
        or not isinstance(expected_sha256, str)
        or re.fullmatch(r"[0-9a-f]{64}", expected_sha256) is None
        or expected_size != installer_path.stat().st_size
        or expected_sha256 != file_sha256(installer_path)
    ):
        raise ValueError("Installer manifest is stale or does not match the current installer")
    return expected_sha256


def main() -> int:
    # GitHub Windows runners keep the legacy cp1252 console encoding by
    # default. The operator guidance is intentionally Chinese, so make the
    # diagnostic output portable instead of failing after the record was
    # already written.
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    arguments = parse_args()
    operator = arguments.operator.strip()
    if not operator:
        raise SystemExit("--operator must not be empty")
    version = package_version(ROOT)
    source_commit = git_head(ROOT)
    installer_name = f"DeskLinkSetup-{version}-x64.exe"
    installer_path = ROOT / "dist" / "windows" / installer_name
    manifest_path = ROOT / "dist" / "windows" / "windows-installer-manifest.json"
    if not installer_path.is_file() or not manifest_path.is_file():
        raise SystemExit("Build the Windows candidate before creating its acceptance record")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    try:
        installer_sha256 = validate_candidate_manifest(
            manifest,
            version=version,
            source_commit=source_commit,
            installer_name=installer_name,
            installer_path=installer_path,
        )
    except (OSError, ValueError) as error:
        raise SystemExit(str(error)) from error
    record = {
        "schema": MANUAL_ACCEPTANCE_SCHEMA,
        "product": "DeskLink Windows acceptance",
        "version": version,
        "source_commit": source_commit,
        "installer": {"file_name": installer_name, "sha256": installer_sha256},
        "operator": operator,
        "recorded_at_utc": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "checks": {check_id: False for check_id in MANUAL_CHECK_IDS},
        "notes": {check_id: "" for check_id in MANUAL_CHECK_IDS},
        "environment": {
            "controller_os": "",
            "host_os": "",
            "controller_arch": "x64",
            "host_arch": "x64",
            "network_modes": [],
            "webview2_verified": False,
        },
        "installation": {
            "fresh_windows_account": False,
            "install_upgrade_uninstall": False,
            "smartscreen_result": "",
        },
        "long_soak": {
            "duration_seconds": 0,
            "sample_interval_seconds": 1800,
            "metrics_recorded": False,
            "diagnostic_exported": False,
        },
    }
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(
        json.dumps(record, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(f"Acceptance record template: {arguments.output.resolve()}")
    print(
        "完成真实验收后填写 checks、notes、environment、installation 和 long_soak，"
        "再运行 check-windows-release-ready.py。"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
