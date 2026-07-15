#!/usr/bin/env python3
"""Build a self-contained per-user DeskLink installer for Windows x64."""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tomllib
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-pc-windows-msvc"


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
    verified_application_sha256 = str(release_report["release"]["sha256"])
    if sha256(application) != verified_application_sha256:
        raise SystemExit("Windows UI payload changed after release verification")

    should_sign = signing_requested()
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

    package = tomllib.loads(
        (ROOT / "tools" / "windows-installer" / "Cargo.toml").read_text(encoding="utf-8")
    )["package"]
    version = package["version"]
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
