#!/usr/bin/env python3
"""Create a candidate-bound Windows manual acceptance record template."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
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


def main() -> int:
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
    installer = manifest.get("installer")
    if (
        manifest.get("version") != version
        or manifest.get("source_commit") != source_commit
        or manifest.get("source_dirty") is not False
        or not isinstance(installer, dict)
        or installer.get("file_name") != installer_name
        or installer.get("sha256") != file_sha256(installer_path)
    ):
        raise SystemExit("Installer manifest is stale or does not match the current source commit")
    record = {
        "schema": 1,
        "product": "DeskLink Windows acceptance",
        "version": version,
        "source_commit": source_commit,
        "installer": {"file_name": installer_name, "sha256": installer["sha256"]},
        "operator": operator,
        "recorded_at_utc": datetime.now(timezone.utc).isoformat().replace("+00:00", "Z"),
        "checks": {check_id: False for check_id in MANUAL_CHECK_IDS},
        "notes": {check_id: "" for check_id in MANUAL_CHECK_IDS},
    }
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(
        json.dumps(record, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(f"Acceptance record template: {arguments.output.resolve()}")
    print("完成真实验收后，将 checks 中对应项目改为 true，再运行 check-windows-release-ready.py。")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
