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
import tomllib
import time
from pathlib import Path

SCRIPTS_DIRECTORY = Path(__file__).resolve().parent
if str(SCRIPTS_DIRECTORY) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIRECTORY))

from windows_native_build_env import (
    prepare_windows_native_build_environment,
    prepare_windows_release_environment,
)


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
    package = tomllib.loads(
        (ROOT / "tools" / "windows-installer" / "Cargo.toml").read_text(encoding="utf-8")
    )["package"]
    version = str(package["version"])
    enforce_release_ref(version)
    should_sign = signing_requested()
    enforce_signing_policy(
        requested=should_sign,
        required=arguments.require_signing
        or environment_flag("DESKLINK_REQUIRE_SIGNING"),
    )

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
    manifest_path = destination.parent / "windows-installer-manifest.json"
    manifest_path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )

    size_mib = destination.stat().st_size / (1024 * 1024)
    signature = "signed and verified" if should_sign else "unsigned"
    print(f"Built {destination} ({size_mib:.1f} MiB, {signature})")
    print(f"Manifest: {manifest_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
