#!/usr/bin/env python3
"""Validate and publish a signed DeskLink Windows GitHub Release."""

from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
VERSION_PATTERN = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")
COMMIT_PATTERN = re.compile(r"^[0-9a-f]{40}$")


@dataclass(frozen=True)
class ReleasePayload:
    version: str
    source_commit: str
    installer: Path
    manifest: Path
    verification: Path
    sha256: str


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def read_json(path: Path) -> dict[str, object]:
    if not path.is_file():
        raise ValueError(f"Required release file is missing: {path}")
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"Release JSON must be an object: {path}")
    return value


def validate_release_payload(root: Path, tag: str) -> ReleasePayload:
    manifest_path = root / "dist" / "windows" / "windows-installer-manifest.json"
    verification_path = (
        root / "dist" / "windows" / "windows-release-verification.json"
    )
    manifest = read_json(manifest_path)
    verification = read_json(verification_path)

    version = manifest.get("version")
    if not isinstance(version, str) or not VERSION_PATTERN.fullmatch(version):
        raise ValueError("Windows installer manifest contains an invalid version")
    if tag != f"v{version}":
        raise ValueError(
            f"Release tag {tag!r} does not match signed payload version {version!r}"
        )
    if manifest.get("schema") != 1 or manifest.get("passed") is not True:
        raise ValueError("Windows installer manifest did not pass release validation")
    if manifest.get("signed") is not True:
        raise ValueError("Unsigned Windows installers cannot become a GitHub Release")
    source_commit = manifest.get("source_commit")
    if not isinstance(source_commit, str) or not COMMIT_PATTERN.fullmatch(source_commit):
        raise ValueError("Windows installer manifest contains an invalid source commit")
    if manifest.get("source_dirty") is not False:
        raise ValueError("Signed Windows releases require a clean source checkout")
    verification_source_commit = verification.get("source_commit")
    if verification_source_commit != source_commit or verification.get("source_dirty") is not False:
        raise ValueError("Windows release verification does not match the clean source commit")
    github_sha = os.environ.get("GITHUB_SHA", "").strip().lower()
    if github_sha and github_sha != source_commit:
        raise ValueError("Release source commit does not match GITHUB_SHA")

    installer_value = manifest.get("installer")
    if not isinstance(installer_value, dict):
        raise ValueError("Windows installer manifest is missing installer metadata")
    expected_name = f"DeskLinkSetup-{version}-x64.exe"
    if installer_value.get("file_name") != expected_name:
        raise ValueError("Windows installer name does not match the release version")
    installer_path = root / "dist" / "windows" / expected_name
    if not installer_path.is_file():
        raise ValueError(f"Signed Windows installer is missing: {installer_path}")
    expected_size = installer_value.get("size_bytes")
    if expected_size != installer_path.stat().st_size:
        raise ValueError("Windows installer size differs from the signed manifest")
    expected_sha256 = installer_value.get("sha256")
    actual_sha256 = file_sha256(installer_path)
    if expected_sha256 != actual_sha256:
        raise ValueError("Windows installer hash differs from the signed manifest")

    if verification.get("passed") is not True or verification.get("version") != version:
        raise ValueError("Windows release verification does not match the installer")
    if verification.get("custom_protocol") is not True:
        raise ValueError("Windows release verification did not use custom protocol")
    scope = verification.get("release_scope")
    if not isinstance(scope, dict) or scope != manifest.get("release_scope"):
        raise ValueError("Windows release scope does not match the installer")
    if (
        scope.get("target") != "windows-10/11-x64"
        or scope.get("macos_release") is not False
        or scope.get("mobile_release") is not False
    ):
        raise ValueError("Windows release scope includes a non-Windows product")
    return ReleasePayload(
        version=version,
        source_commit=source_commit,
        installer=installer_path,
        manifest=manifest_path,
        verification=verification_path,
        sha256=actual_sha256,
    )


def release_notes(payload: ReleasePayload) -> str:
    return (
        f"DeskLink {payload.version} Windows x64 正式版本。\n\n"
        "- 安装器和内置应用均已通过 Authenticode 签名验证。\n"
        "- 安装范围仅限当前 Windows 用户，不需要管理员权限。\n"
        "- 覆盖升级会保留设备身份、已批准设备和本机设置。\n\n"
        f"源码提交：`{payload.source_commit}`\n"
        f"安装器 SHA-256：`{payload.sha256}`\n"
    )


def publish(payload: ReleasePayload, *, repository: str, tag: str) -> None:
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("GITHUB_REPOSITORY is invalid")
    with tempfile.TemporaryDirectory(prefix="desklink-release-") as directory:
        notes_path = Path(directory) / "release-notes.md"
        notes_path.write_text(release_notes(payload), encoding="utf-8")
        command = [
            "gh",
            "release",
            "create",
            tag,
            str(payload.installer),
            str(payload.manifest),
            str(payload.verification),
            "--repo",
            repository,
            "--verify-tag",
            "--latest",
            "--title",
            f"DeskLink {payload.version}",
            "--notes-file",
            str(notes_path),
        ]
        subprocess.run(command, cwd=ROOT, check=True)


def main() -> int:
    if os.environ.get("GITHUB_REF_TYPE") != "tag":
        raise SystemExit("Windows releases can only be published from a Git tag")
    tag = os.environ.get("GITHUB_REF_NAME", "").strip()
    repository = os.environ.get("GITHUB_REPOSITORY", "").strip()
    try:
        payload = validate_release_payload(ROOT, tag)
        publish(payload, repository=repository, tag=tag)
    except (ValueError, json.JSONDecodeError) as error:
        raise SystemExit(str(error)) from error
    print(f"Published signed DeskLink {payload.version} Windows release")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
