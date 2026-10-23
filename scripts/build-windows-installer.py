#!/usr/bin/env python3
"""Build a self-contained per-user DeskLink installer for Windows x64."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

SCRIPTS_DIRECTORY = Path(__file__).resolve().parent
if str(SCRIPTS_DIRECTORY) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIRECTORY))

from windows_native_build_env import (
    prepare_windows_native_build_environment,
    prepare_windows_release_environment,
)
from cargo_manifest import package_version


ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-pc-windows-msvc"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Build the verified DeskLink Windows x64 installer."
    )
    parser.add_argument(
        "--require-signing",
        action="store_true",
        help="fail before building unless a signing identity is configured",
    )
    parser.add_argument(
        "--require-update-signing",
        action="store_true",
        help="require an independent update manifest signature (current-user release key)",
    )
    parser.add_argument(
        "--public-test",
        action="store_true",
        help="build an explicitly unsigned public test update from a clean commit; requires independent update signing",
    )
    return parser.parse_args()


def run(
    command: list[str],
    *,
    env: dict[str, str] | None = None,
    cwd: Path = ROOT,
) -> None:
    print("+", subprocess.list2cmdline(command), flush=True)
    subprocess.run(command, cwd=cwd, env=env, check=True)


def signing_requested() -> bool:
    return any(
        os.environ.get(name)
        for name in (
            "DESKLINK_ARTIFACT_SIGNING_DLIB",
            "DESKLINK_ARTIFACT_SIGNING_METADATA",
            "DESKLINK_SIGN_CERT_SHA1",
        )
    )


def environment_flag(name: str) -> bool:
    value = os.environ.get(name, "").strip().lower()
    if not value:
        return False
    if value in {"1", "true", "yes", "on"}:
        return True
    if value in {"0", "false", "no", "off"}:
        return False
    raise SystemExit(
        f"{name} must be one of 1/0, true/false, yes/no, or on/off."
    )


def enforce_signing_policy(*, requested: bool, required: bool) -> None:
    if required and not requested:
        raise SystemExit(
            "Trusted Windows signing is required for this build, but no signing "
            "identity is configured. Set the Artifact Signing variables or "
            "DESKLINK_SIGN_CERT_SHA1."
        )


def public_test_source(root: Path) -> str:
    """Bind public test builds to the actual clean checkout, never a CI hint."""
    def git(*arguments: str) -> str:
        return subprocess.run(
            ["git", *arguments], cwd=root, capture_output=True, text=True,
            encoding="utf-8", check=True,
        ).stdout.strip()
    commit = git("rev-parse", "HEAD")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise SystemExit("Public test source HEAD is invalid")
    if git("status", "--porcelain", "--untracked-files=all"):
        raise SystemExit("Public test builds require a clean source checkout")
    hinted = os.environ.get("GITHUB_SHA", "").strip().lower()
    if hinted and hinted != commit:
        raise SystemExit("GITHUB_SHA does not match actual public test HEAD")
    return commit


def enforce_public_test_policy(*, public_test: bool, requested: bool, required: bool) -> None:
    if public_test and (requested or required):
        raise SystemExit("--public-test is explicitly unsigned and cannot be combined with Authenticode signing options; formal publishing is unchanged")


def enforce_release_ref(version: str) -> None:
    """Prevent a tagged release from publishing a differently-versioned binary."""
    if os.environ.get("GITHUB_REF_TYPE", "").strip() != "tag":
        return
    actual = os.environ.get("GITHUB_REF_NAME", "").strip()
    expected = f"v{version}"
    if actual != expected:
        raise SystemExit(
            f"Release tag {actual!r} does not match the Windows version {version!r}; "
            f"expected {expected!r}."
        )


def sign(artifact: Path) -> None:
    run(
        [
            sys.executable,
            str(ROOT / "scripts" / "sign-windows-artifact.py"),
            str(artifact),
        ]
    )


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def main() -> int:
    if os.name != "nt":
        raise SystemExit("The Windows installer must be built on Windows.")
    prepare_windows_native_build_environment()
    prepare_windows_release_environment()

    arguments = parse_args()
    version = package_version(ROOT / "tools" / "windows-installer" / "Cargo.toml")
    enforce_release_ref(version)
    should_sign = signing_requested()
    formal_signing_required = arguments.require_signing or environment_flag("DESKLINK_REQUIRE_SIGNING")
    enforce_public_test_policy(public_test=arguments.public_test, requested=should_sign, required=formal_signing_required)
    public_source = public_test_source(ROOT) if arguments.public_test else None
    enforce_signing_policy(
        requested=should_sign,
        required=formal_signing_required,
    )

    # Unlock before the build so a missing/mismatched release key fails early.
    # An unsigned manifest is never accepted by the in-app updater.
    from windows_update_signing import configured_key_path, load_release_key, sign_manifest
    update_key_path = configured_key_path()
    update_signing_required = arguments.require_update_signing or formal_signing_required or arguments.public_test
    update_key = None
    if update_key_path.is_file() or update_signing_required or os.environ.get("DESKLINK_UPDATE_SIGNING_KEY"):
        try:
            update_key = load_release_key(update_key_path)
        except (OSError, ValueError) as error:
            raise SystemExit(f"Update signing key unavailable: {error}") from error

    run([sys.executable, str(ROOT / "scripts" / "verify-windows-release.py")])
    release = ROOT / "target" / TARGET / "release"
    application = release / "desklink-windows-ui.exe"
    if not application.is_file():
        raise SystemExit(f"Windows UI payload was not produced: {application}")
    release_report = json.loads(
        (ROOT / "dist" / "windows" / "windows-release-verification.json").read_text(
            encoding="utf-8"
        )
    )
    source_commit = release_report.get("source_commit")
    source_dirty = release_report.get("source_dirty")
    if not isinstance(source_commit, str) or not re.fullmatch(
        r"[0-9a-f]{40}", source_commit
    ):
        raise SystemExit("Windows release verification has no valid source commit")
    if not isinstance(source_dirty, bool):
        raise SystemExit("Windows release verification has no source checkout status")
    if should_sign and source_dirty:
        raise SystemExit("Signed Windows releases require a clean source checkout")
    if arguments.public_test and (
        source_dirty or source_commit != public_source
        or release_report.get("version") != version
        or release_report.get("passed") is not True
        or release_report.get("custom_protocol") is not True
    ):
        raise SystemExit("Public test verification must match the actual clean source/version/custom protocol")
    release_scope = release_report.get("release_scope")
    if not isinstance(release_scope, dict):
        raise SystemExit("Windows release verification has no release scope")
    verified_application_sha256 = str(release_report["release"]["sha256"])
    if sha256(application) != verified_application_sha256:
        raise SystemExit("Windows UI payload changed after release verification")

    application_payload = application
    if should_sign:
        application_payload = application.with_name("DeskLink.signed-payload.exe")
        shutil.copy2(application, application_payload)
        sign(application_payload)
    else:
        print(
            "Signing skipped: configure Artifact Signing or DESKLINK_SIGN_CERT_SHA1 "
            "for a release build.",
            flush=True,
        )

    installer_environment = os.environ.copy()
    installer_environment["DESKLINK_WINDOWS_UI_PAYLOAD"] = str(
        application_payload.resolve()
    )
    installer_environment["DESKLINK_WINDOWS_UI_PAYLOAD_SHA256"] = sha256(
        application_payload
    )
    run(
        [
            "cargo",
            "build",
            "--release",
            "--target",
            TARGET,
            "--package",
            "desklink-installer",
            "--bin",
            "desklink-installer",
            "--features",
            "embedded-payload",
        ],
        env=installer_environment,
    )

    built_installer = ROOT / "target" / TARGET / "release" / "desklink-installer.exe"
    if not built_installer.is_file():
        raise SystemExit(f"Installer executable was not produced: {built_installer}")

    destination = ROOT / "dist" / "windows" / f"DeskLinkSetup-{version}-x64.exe"
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(built_installer, destination)

    if should_sign:
        sign(destination)

    payload_bytes = application_payload.read_bytes()
    installer_bytes = destination.read_bytes()
    payload_offset = installer_bytes.find(payload_bytes)
    if payload_offset < 0:
        raise SystemExit("Final installer does not contain the exact verified application payload")

    manifest = {
        "schema": 1,
        "version": str(version),
        "source_commit": source_commit,
        "source_dirty": source_dirty,
        "release_scope": release_scope,
        "target": TARGET,
        "update_mode": "perUserV1",
        "signed": should_sign,
        "application": {
            "file_name": "DeskLink.exe",
            "size_bytes": len(payload_bytes),
            "sha256": sha256(application_payload),
            "source_release_sha256": verified_application_sha256,
        },
        "installer": {
            "file_name": destination.name,
            "size_bytes": len(installer_bytes),
            "sha256": sha256(destination),
            "embedded_payload_offset": payload_offset,
        },
        "passed": True,
        "completed_at_unix_s": int(time.time()),
    }
    if arguments.public_test:
        if public_test_source(ROOT) != public_source:
            raise SystemExit("Public test source changed during the build")
        manifest["release_channel"] = "publicTest"
    manifest_path = destination.parent / "windows-installer-manifest.json"
    manifest_path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )

    signature_path = manifest_path.with_name(manifest_path.name + ".sig")
    if update_key is not None:
        sign_manifest(manifest_path, update_key)
        print(f"Independent update signature: {signature_path}")
    else:
        # Prevent an earlier build's signature from traveling with new bytes.
        signature_path.unlink(missing_ok=True)
        print("No update release key configured: this build cannot be offered by the in-app updater.")

    size_mib = destination.stat().st_size / (1024 * 1024)
    signature = "signed and verified" if should_sign else "unsigned"
    print(f"Built {destination} ({size_mib:.1f} MiB, {signature})")
    print(f"Manifest: {manifest_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
