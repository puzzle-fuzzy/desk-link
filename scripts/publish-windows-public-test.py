#!/usr/bin/env python3
"""Publish an explicitly unsigned, independently authorized Windows public test.

This is a separate policy from publish-windows-release.py. It never creates a
formal-readiness assertion or bypasses that script's Authenticode requirements.
All network writes require an explicit draft or publish subcommand.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile

SCRIPTS_DIRECTORY = Path(__file__).resolve().parent
if str(SCRIPTS_DIRECTORY) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIRECTORY))
from windows_update_signing import read_public_key, verify_manifest_signature

ROOT = SCRIPTS_DIRECTORY.parent
REPOSITORY = "puzzle-fuzzy/desk-link"
TARGET = "x86_64-pc-windows-msvc"
VERSION = re.compile(r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
EXPECTED_SCOPE = {"target": "windows-10/11-x64", "macos_release": False, "mobile_release": False}


@dataclass(frozen=True)
class PublicTestPayload:
    root: Path
    version: str
    source_commit: str
    installer: Path
    manifest: Path
    signature: Path
    verification: Path
    sha256: str
    asset_hashes: tuple[str, ...]

    @property
    def files(self) -> tuple[Path, ...]:
        return self.installer, self.manifest, self.signature, self.verification


def file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1_048_576), b""):
            digest.update(block)
    return digest.hexdigest()


def git(root: Path, *arguments: str) -> str:
    return subprocess.run(["git", *arguments], cwd=root, capture_output=True, text=True, encoding="utf-8", check=True).stdout.strip()


def clean_head(root: Path) -> str:
    commit = git(root, "rev-parse", "HEAD")
    if not COMMIT.fullmatch(commit):
        raise ValueError("Invalid actual source HEAD")
    if git(root, "status", "--porcelain", "--untracked-files=all"):
        raise ValueError("Public test publishing requires a clean source checkout")
    return commit


def read_json(path: Path) -> dict:
    if not path.is_file() or path.stat().st_size > 1_000_000:
        raise ValueError(f"Missing or oversized public test report: {path.name}")
    value = json.loads(path.read_bytes())
    if not isinstance(value, dict):
        raise ValueError(f"Public test report is not an object: {path.name}")
    return value


def validate_public_test_payload(root: Path, tag: str) -> PublicTestPayload:
    actual_commit = clean_head(root)
    if not tag.startswith("v") or not VERSION.fullmatch(tag[1:]):
        raise ValueError("Public test tag must have the form v<major>.<minor>.<patch>")
    if git(root, "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}") != actual_commit:
        raise ValueError("Public test tag does not match actual source HEAD")
    dist = root / "dist/windows"
    manifest_path = dist / "windows-installer-manifest.json"
    signature_path = dist / "windows-installer-manifest.json.sig"
    if not manifest_path.is_file() or manifest_path.stat().st_size > 100_000:
        raise ValueError("Public test manifest is missing or oversized")
    # Authenticate the raw bytes before interpreting paths, claims or hashes.
    raw = manifest_path.read_bytes()
    verify_manifest_signature(raw, signature_path.read_bytes(), read_public_key(root / "apps/windows-ui/src-tauri/update-public-key.hex"))
    manifest = json.loads(raw)
    if not isinstance(manifest, dict):
        raise ValueError("Public test manifest must be an object")
    version = manifest.get("version")
    if version != tag[1:]:
        raise ValueError("Public test tag does not match manifest version")
    if any(int(part) > 9_007_199_254_740_991 for part in version.split(".")):
        raise ValueError("Public test version exceeds the installed client's numeric range")
    allowed_manifest = {"schema", "version", "source_commit", "source_dirty", "release_scope", "target", "update_mode", "signed", "application", "installer", "passed", "completed_at_unix_s", "release_channel"}
    if set(manifest) - allowed_manifest:
        raise ValueError("Public test manifest contains unreviewed metadata fields")
    if "completed_at_unix_s" in manifest and (type(manifest["completed_at_unix_s"]) is not int or manifest["completed_at_unix_s"] < 0):
        raise ValueError("Public test build timestamp is invalid")
    if type(manifest.get("schema")) is not int or manifest.get("schema") != 1 or manifest.get("passed") is not True:
        raise ValueError("Public test manifest did not pass build verification")
    if manifest.get("release_channel") != "publicTest" or manifest.get("signed") is not False:
        raise ValueError("This publisher accepts only explicitly unsigned publicTest manifests")
    if manifest.get("target") != TARGET or manifest.get("update_mode") != "perUserV1" or manifest.get("release_scope") != EXPECTED_SCOPE:
        raise ValueError("Public test target, scope or update mode is invalid")
    if manifest.get("source_commit") != actual_commit or manifest.get("source_dirty") is not False:
        raise ValueError("Public test manifest must match the actual clean source commit")
    installer_value = manifest.get("installer")
    application_value = manifest.get("application")
    if not isinstance(installer_value, dict) or not isinstance(application_value, dict):
        raise ValueError("Public test installer/application metadata is missing")
    if set(installer_value) - {"file_name", "size_bytes", "sha256", "embedded_payload_offset"} or set(application_value) - {"file_name", "size_bytes", "sha256", "source_release_sha256"}:
        raise ValueError("Public test binary metadata contains unreviewed fields")
    name = f"DeskLinkSetup-{version}-x64.exe"
    if installer_value.get("file_name") != name or application_value.get("file_name") != "DeskLink.exe":
        raise ValueError("Public test installer/application filename is not canonical")
    installer = dist / name
    application = root / "target" / TARGET / "release/desklink-windows-ui.exe"
    for label, path, data in [("installer", installer, installer_value), ("application", application, application_value)]:
        size, digest = data.get("size_bytes"), data.get("sha256")
        if type(size) is not int or not 0 < size <= 512 * 1_024 * 1_024 or not isinstance(digest, str) or not SHA256.fullmatch(digest):
            raise ValueError(f"Public test {label} size/hash metadata is invalid")
        if not path.is_file() or path.stat().st_size != size or file_sha256(path) != digest:
            raise ValueError(f"Public test {label} actual size/hash does not match")
    if installer_value["size_bytes"] < 1_000_000:
        raise ValueError("Public test installer is too small for the client update policy")
    offset, length = installer_value.get("embedded_payload_offset"), application_value["size_bytes"]
    if type(offset) is not int or offset < 0 or offset + length > installer_value["size_bytes"]:
        raise ValueError("Public test embedded application bounds are invalid")
    with installer.open("rb") as source:
        source.seek(offset)
        digest = hashlib.sha256()
        remaining = length
        while remaining:
            block = source.read(min(remaining, 1_048_576))
            if not block:
                raise ValueError("Public test embedded application is truncated")
            digest.update(block)
            remaining -= len(block)
    if digest.hexdigest() != application_value["sha256"]:
        raise ValueError("Public test final installer does not embed the verified application")
    verification_path = dist / "windows-release-verification.json"
    report = read_json(verification_path)
    if type(report.get("schema")) is not int or report.get("schema") != 1 or report.get("version") != version or report.get("passed") is not True or report.get("custom_protocol") is not True:
        raise ValueError("Public test verification version/custom protocol/passed is invalid")
    if report.get("source_commit") != actual_commit or report.get("source_dirty") is not False or report.get("release_scope") != EXPECTED_SCOPE:
        raise ValueError("Public test verification does not match clean source/scope")
    verified = report.get("release")
    if not isinstance(verified, dict) or verified.get("sha256") != application_value["sha256"] or verified.get("size_bytes") != length or application_value.get("source_release_sha256") != application_value["sha256"]:
        raise ValueError("Public test application does not match the exact verified build")
    if clean_head(root) != actual_commit:
        raise ValueError("Public test source changed during validation")
    # The build report includes local absolute paths and stays private. Publish
    # only this explicit allowlist, never arbitrary logs/report dictionaries.
    public_verification = dist / "windows-public-test-verification.json"
    public_verification.write_text(json.dumps({
        "schema": 1, "version": version, "target": TARGET,
        "release_channel": "publicTest", "source_commit": actual_commit,
        "source_dirty": False, "signed": False, "ready": False,
        "installer": {"file_name": name, "size_bytes": installer_value["size_bytes"], "sha256": installer_value["sha256"]},
        "manifest": {"file_name": manifest_path.name, "sha256": hashlib.sha256(raw).hexdigest(), "signature_verified": True},
        "checks": {"build_verification_passed": True, "custom_protocol": True, "embedded_application_verified": True, "application_sha256": application_value["sha256"]},
        "limitations": ["unsigned_authenticode", "formal_release_acceptance_not_asserted"],
    }, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    files = installer, manifest_path, signature_path, public_verification
    return PublicTestPayload(root, version, actual_commit, *files, installer_value["sha256"], tuple(file_sha256(path) for path in files))


def release_notes(payload: PublicTestPayload) -> str:
    return (
        f"DeskLink {payload.version} Windows x64 公开测试版（未 Authenticode 签名）。\n\n"
        "这是独立 Ed25519 更新密钥授权的公开测试分发，不是正式发布，"
        "不代表正式发布 readiness、双机长稳或全部人工验收已通过。"
        "Windows 可能显示未知发布者提示。\n\n"
        "为兼容已安装的 0.1.92，本测试使用 GitHub 非 prerelease 的 latest 分发入口；"
        "这仅是现有更新发现协议的要求，不改变本版本的测试性质。\n\n"
        "安装将结束当前连接、保留用户数据及开机启动设置，成功后重新打开客户端。\n\n"
        f"源码提交：`{payload.source_commit}`\n安装器 SHA-256：`{payload.sha256}`\n"
    )


def gh_json(root: Path, endpoint: str) -> dict:
    result = subprocess.run(["gh", "api", endpoint], cwd=root, capture_output=True, text=True, encoding="utf-8", check=True)
    if len(result.stdout) > 1_000_000:
        raise ValueError("GitHub response exceeds its size limit")
    value = json.loads(result.stdout)
    if not isinstance(value, dict):
        raise ValueError("GitHub response is not an object")
    return value


def verify_remote_tag(payload: PublicTestPayload, tag: str) -> None:
    value = gh_json(payload.root, f"repos/{REPOSITORY}/git/ref/tags/{tag}").get("object", {})
    for _ in range(5):
        if value.get("type") == "commit" and value.get("sha") == payload.source_commit:
            return
        if value.get("type") != "tag" or not isinstance(value.get("sha"), str) or not COMMIT.fullmatch(value["sha"]):
            break
        value = gh_json(payload.root, f"repos/{REPOSITORY}/git/tags/{value['sha']}").get("object", {})
    raise ValueError("Remote public test tag does not resolve to the verified source commit")


def verify_public_repository(root: Path) -> None:
    repository = gh_json(root, f"repos/{REPOSITORY}")
    if repository.get("full_name", "").lower() != REPOSITORY.lower() or repository.get("private") is not False:
        raise ValueError("The fixed update repository must already be public; this tool never changes visibility")


def verify_draft_assets(payload: PublicTestPayload, tag: str) -> None:
    verify_remote_tag(payload, tag)
    verify_public_repository(payload.root)
    # gh handles authenticated draft lookup, including versions where the REST
    # published-release-by-tag endpoint does not expose a draft.
    response = subprocess.run(["gh", "release", "view", tag, "--repo", REPOSITORY, "--json", "tagName,isDraft,isPrerelease,name,body,assets"], cwd=payload.root, capture_output=True, text=True, encoding="utf-8", check=True)
    if len(response.stdout) > 1_000_000:
        raise ValueError("GitHub draft response exceeds its size limit")
    release = json.loads(response.stdout)
    if release.get("tagName") != tag or release.get("isDraft") is not True or release.get("isPrerelease") is not False:
        raise ValueError("Expected an unpublished non-prerelease public test draft")
    if release.get("name") != f"DeskLink {payload.version} 公开测试版（unsigned）" or str(release.get("body", "")).replace("\r\n", "\n").strip() != release_notes(payload).strip():
        raise ValueError("Public test draft must explicitly disclose its unsigned test status")
    assets = release.get("assets")
    expected = {path.name: path for path in payload.files}
    if not isinstance(assets, list) or not all(isinstance(asset, dict) for asset in assets) or len(assets) != len(expected) or {asset.get("name") for asset in assets} != set(expected):
        raise ValueError("Public test draft must contain exactly the four verified assets")
    for asset in assets:
        if asset.get("size") != expected[asset["name"]].stat().st_size:
            raise ValueError("Public test draft asset size does not match")
    # Download the staged bytes using existing gh authentication. No private key
    # or account data is sent, and no release is published by this verification.
    with tempfile.TemporaryDirectory(prefix="desklink-public-test-verify-") as directory:
        command = ["gh", "release", "download", tag, "--repo", REPOSITORY, "--dir", directory]
        for name in expected:
            command.extend(["--pattern", name])
        subprocess.run(command, cwd=payload.root, check=True)
        hashes = dict(zip(expected, payload.asset_hashes, strict=True))
        for name, source in expected.items():
            downloaded = Path(directory) / name
            if not downloaded.is_file() or downloaded.stat().st_size != source.stat().st_size or file_sha256(downloaded) != hashes[name]:
                raise ValueError("Public test draft downloaded bytes do not match local verified assets")


def create_draft(payload: PublicTestPayload, tag: str) -> None:
    verify_remote_tag(payload, tag)
    verify_public_repository(payload.root)
    with tempfile.TemporaryDirectory(prefix="desklink-public-test-notes-") as directory:
        notes = Path(directory) / "release-notes.md"
        notes.write_text(release_notes(payload), encoding="utf-8")
        subprocess.run(["gh", "release", "create", tag, *(str(path) for path in payload.files), "--repo", REPOSITORY, "--verify-tag", "--target", payload.source_commit, "--draft", "--prerelease=false", "--title", f"DeskLink {payload.version} 公开测试版（unsigned）", "--notes-file", str(notes)], cwd=payload.root, check=True)
    verify_draft_assets(payload, tag)


def publish_draft(payload: PublicTestPayload, tag: str) -> None:
    verify_draft_assets(payload, tag)
    # Re-read all source-bound local inputs after remote verification and before
    # the explicit final visibility mutation. Formal publishing stays separate.
    current = validate_public_test_payload(payload.root, tag)
    if current != payload:
        raise ValueError("Public test payload changed before publication")
    subprocess.run(["gh", "release", "edit", tag, "--repo", REPOSITORY, "--draft=false", "--prerelease=false", "--latest"], cwd=payload.root, check=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["validate", "draft", "publish"])
    parser.add_argument("--tag", required=True)
    arguments = parser.parse_args()
    try:
        payload = validate_public_test_payload(ROOT, arguments.tag)
        if arguments.action == "draft":
            create_draft(payload, arguments.tag)
        elif arguments.action == "publish":
            publish_draft(payload, arguments.tag)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"Public test {arguments.action} failed: {error}") from error
    print(f"Public test {arguments.action} complete: {payload.version} ({payload.source_commit})")
    return 0


if __name__ == "__main__":
    sys.stdout.reconfigure(encoding="utf-8")
    raise SystemExit(main())
