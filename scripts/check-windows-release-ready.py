#!/usr/bin/env python3
"""Produce a single, read-only readiness report for a DeskLink Windows release.

The command deliberately separates machine-verifiable gates from two-Windows
acceptance.  It is safe to run on a working checkout and never publishes or
changes the release artifacts.  Use ``--strict`` in a release job when a
non-zero exit code is required for a not-ready candidate.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import time
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
COMMIT_PATTERN = re.compile(r"^[0-9a-f]{40}$")
VERSION_PATTERN = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")
EXPECTED_SCOPE = {
    "target": "windows-10/11-x64",
    "macos_release": False,
    "mobile_release": False,
}
MANUAL_CHECK_IDS = (
    "two_windows_acceptance",
    "long_soak_acceptance",
    "smartscreen_acceptance",
)


def read_json(path: Path) -> dict[str, Any] | None:
    if not path.is_file():
        return None
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None
    return value if isinstance(value, dict) else None


def sha256(path: Path) -> str | None:
    if not path.is_file():
        return None
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def git_head(root: Path) -> str | None:
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
    return value if completed.returncode == 0 and COMMIT_PATTERN.fullmatch(value) else None


def git_dirty(root: Path) -> bool | None:
    completed = subprocess.run(
        ["git", "status", "--porcelain", "--untracked-files=all"],
        cwd=root,
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    if completed.returncode != 0:
        return None
    return bool(completed.stdout.strip())


def git_tag_exists(root: Path, tag: str) -> bool | None:
    completed = subprocess.run(
        ["git", "show-ref", "--verify", "--quiet", f"refs/tags/{tag}"],
        cwd=root,
        check=False,
    )
    return completed.returncode == 0


def package_version(root: Path) -> str | None:
    path = root / "tools" / "windows-installer" / "Cargo.toml"
    try:
        source = path.read_text(encoding="utf-8")
    except OSError:
        return None
    match = re.search(r"(?m)^version\s*=\s*\"([^\"]+)\"", source)
    version = match.group(1) if match else None
    return version if version and VERSION_PATTERN.fullmatch(version) else None


def _check(
    checks: dict[str, dict[str, Any]],
    blockers: list[dict[str, str]],
    check_id: str,
    passed: bool,
    *,
    category: str,
    detail: str,
    severity: str = "P0",
    manual: bool = False,
) -> None:
    checks[check_id] = {
        "passed": passed,
        "category": category,
        "severity": severity,
        "manual": manual,
        "detail": detail,
    }
    if not passed:
        blockers.append(
            {"id": check_id, "severity": severity, "category": category, "detail": detail}
        )


def _fresh_report(
    report: dict[str, Any] | None,
    *,
    now: int,
    max_age_seconds: int,
    label: str,
) -> tuple[bool, str]:
    if report is None:
        return False, f"{label} report is missing or invalid"
    if report.get("passed") is not True:
        return False, f"{label} report did not pass"
    completed = report.get("completed_at_unix_s")
    if not isinstance(completed, int) or isinstance(completed, bool):
        return False, f"{label} report has no completion timestamp"
    age = now - completed
    if age < -600 or age > max_age_seconds:
        return False, f"{label} report is stale ({max(age, 0)} seconds old)"
    return True, f"{label} report passed and is fresh"


def evaluate_preflight(
    *,
    version: str | None,
    head: str | None,
    dirty: bool | None,
    verification: dict[str, Any] | None,
    manifest: dict[str, Any] | None,
    installer_path: Path,
    tag_exists: bool | None,
    relay_report: dict[str, Any] | None,
    diagnostics_report: dict[str, Any] | None,
    now: int | None = None,
    manual_checks: dict[str, bool] | None = None,
) -> dict[str, Any]:
    now = int(time.time()) if now is None else now
    manual_checks = manual_checks or {}
    checks: dict[str, dict[str, Any]] = {}
    blockers: list[dict[str, str]] = []

    _check(
        checks,
        blockers,
        "package_version",
        version is not None,
        category="release",
        detail=f"Windows installer package version: {version or 'unavailable'}",
    )
    expected_commit = manifest.get("source_commit") if manifest else None
    verification_commit = verification.get("source_commit") if verification else None
    commits_match = (
        isinstance(expected_commit, str)
        and COMMIT_PATTERN.fullmatch(expected_commit) is not None
        and expected_commit == verification_commit == head
    )
    _check(
        checks,
        blockers,
        "source_commit_match",
        commits_match,
        category="provenance",
        detail="Verification, installer manifest and current HEAD must use one commit SHA",
    )
    _check(
        checks,
        blockers,
        "source_clean",
        dirty is False
        and (manifest or {}).get("source_dirty") is False
        and (verification or {}).get("source_dirty") is False,
        category="provenance",
        detail="Signed release artifacts require a clean checkout",
    )
    _check(
        checks,
        blockers,
        "verification_passed",
        verification is not None
        and verification.get("passed") is True
        and verification.get("version") == version
        and verification.get("custom_protocol") is True
        and verification.get("release_scope") == EXPECTED_SCOPE,
        category="release",
        detail="Windows verification must pass with the custom protocol and Windows-only scope",
    )
    _check(
        checks,
        blockers,
        "installer_manifest_passed",
        manifest is not None
        and manifest.get("schema") == 1
        and manifest.get("passed") is True
        and manifest.get("version") == version
        and manifest.get("release_scope") == EXPECTED_SCOPE,
        category="release",
        detail="Installer manifest must match the Windows-only release scope",
    )

    installer_meta = manifest.get("installer") if manifest else None
    expected_name = f"DeskLinkSetup-{version}-x64.exe" if version else ""
    installer_ok = (
        isinstance(installer_meta, dict)
        and installer_meta.get("file_name") == expected_name
        and installer_path.is_file()
        and installer_meta.get("size_bytes") == installer_path.stat().st_size
        and installer_meta.get("sha256") == sha256(installer_path)
    )
    _check(
        checks,
        blockers,
        "installer_integrity",
        installer_ok,
        category="artifact",
        detail="Installer filename, size and SHA-256 must match its manifest",
    )
    _check(
        checks,
        blockers,
        "authenticode_signature",
        manifest is not None and manifest.get("signed") is True,
        category="trust",
        detail="The formal release installer must be Authenticode-signed",
    )
    _check(
        checks,
        blockers,
        "release_tag",
        tag_exists is True,
        category="release",
        detail=f"Annotated release tag v{version or '?'} must exist before publishing",
    )

    relay_ok, relay_detail = _fresh_report(
        relay_report, now=now, max_age_seconds=24 * 60 * 60, label="Managed relay verification"
    )
    _check(
        checks,
        blockers,
        "managed_relay_evidence",
        relay_ok,
        category="operations",
        detail=relay_detail,
        severity="P1",
    )
    diagnostics_ok, diagnostics_detail = _fresh_report(
        diagnostics_report,
        now=now,
        max_age_seconds=2 * 60 * 60,
        label="Managed diagnostics audit",
    )
    _check(
        checks,
        blockers,
        "managed_diagnostics_evidence",
        diagnostics_ok,
        category="operations",
        detail=diagnostics_detail,
        severity="P1",
    )

    warnings = [
        {
            "id": "relay_high_availability",
            "severity": "P1",
            "detail": "当前中继仍是单节点；首个候选版可观察，但正式高可用上线前应补第二节点和故障切换。",
        }
    ]
    for check_id in MANUAL_CHECK_IDS:
        manual_passed = manual_checks.get(check_id) is True
        labels = {
            "two_windows_acceptance": "两台 Windows 配对、控制、视频和输入验收",
            "long_soak_acceptance": "断网恢复、双屏、剪贴板/文件和长时间运行验收",
            "smartscreen_acceptance": "全新 Windows 账户的安装、升级和 SmartScreen 验收",
        }
        _check(
            checks,
            blockers,
            check_id,
            manual_passed,
            category="manual",
            detail=(
                f"{labels[check_id]}："
                + ("已记录通过" if manual_passed else "等待发布负责人在真实 Windows 电脑上记录结果")
            ),
            severity="P0" if check_id == "two_windows_acceptance" else "P1",
            manual=True,
        )

    return {
        "schema": 1,
        "version": version,
        "source_commit": head,
        "source_dirty": dirty,
        "ready": not blockers,
        "blockers": blockers,
        "warnings": warnings,
        "checks": checks,
        "completed_at_unix_s": now,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--report",
        type=Path,
        default=ROOT / "dist" / "windows" / "windows-release-readiness.json",
    )
    parser.add_argument(
        "--manual-json",
        type=Path,
        help="Optional JSON object marking manual check ids as true after real acceptance",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="Exit 1 when any blocker remains; default only writes the report",
    )
    return parser.parse_args()


def main() -> int:
    arguments = parse_args()
    version = package_version(ROOT)
    installer = ROOT / "dist" / "windows" / (
        f"DeskLinkSetup-{version}-x64.exe" if version else "DeskLinkSetup-unknown-x64.exe"
    )
    manual_checks: dict[str, bool] = {}
    if arguments.manual_json:
        value = read_json(arguments.manual_json)
        if value is None:
            raise SystemExit(f"Manual acceptance JSON is missing or invalid: {arguments.manual_json}")
        manual_checks = {key: value.get(key) is True for key in MANUAL_CHECK_IDS}
    report = evaluate_preflight(
        version=version,
        head=git_head(ROOT),
        dirty=git_dirty(ROOT),
        verification=read_json(ROOT / "dist" / "windows" / "windows-release-verification.json"),
        manifest=read_json(ROOT / "dist" / "windows" / "windows-installer-manifest.json"),
        installer_path=installer,
        tag_exists=git_tag_exists(ROOT, f"v{version}" if version else "vunknown"),
        relay_report=read_json(ROOT / "dist" / "windows" / "managed-relay-verification.json"),
        diagnostics_report=read_json(ROOT / "dist" / "linux" / "managed-diagnostics-audit.json"),
        manual_checks=manual_checks,
    )
    arguments.report.parent.mkdir(parents=True, exist_ok=True)
    arguments.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 1 if arguments.strict and not report["ready"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
