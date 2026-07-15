#!/usr/bin/env python3
"""Build a self-contained per-user DeskLink installer for Windows x64."""

from __future__ import annotations

import hashlib
import os
import shutil
import subprocess
import sys
import tomllib
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

    run([sys.executable, str(ROOT / "scripts" / "generate-windows-assets.py")])
    windows_ui = ROOT / "apps" / "windows-ui"
    run(["bun", "install", "--frozen-lockfile"], cwd=windows_ui)
    run(["bun", "run", "build"], cwd=windows_ui)
    run(
        [
            "cargo",
            "build",
            "--release",
            "--target",
            TARGET,
            "--package",
            "desklink-windows-ui",
            "--bin",
            "desklink-windows-ui",
            "--features",
            "custom-protocol",
        ]
    )
    run(
        [
            "cargo",
            "build",
            "--release",
            "--target",
            TARGET,
            "--package",
            "desklink-windows",
            "--bin",
            "desklink-windows",
            "--features",
            "installer-gui",
        ]
    )

    release = ROOT / "target" / TARGET / "release"
    application = release / "desklink-windows-ui.exe"
    host = release / "desklink-windows.exe"
    if not application.is_file():
        raise SystemExit(f"Windows UI payload was not produced: {application}")
    if not host.is_file():
        raise SystemExit(f"Windows host payload was not produced: {host}")

    should_sign = signing_requested()
    application_payload = application
    host_payload = host
    if should_sign:
        application_payload = application.with_name("DeskLink.signed-payload.exe")
        host_payload = host.with_name("desklink-windows.signed-payload.exe")
        shutil.copy2(application, application_payload)
        shutil.copy2(host, host_payload)
        sign(application_payload)
        sign(host_payload)
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
    installer_environment["DESKLINK_WINDOWS_HOST_PAYLOAD"] = str(host_payload.resolve())
    installer_environment["DESKLINK_WINDOWS_HOST_PAYLOAD_SHA256"] = sha256(host_payload)
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

    size_mib = destination.stat().st_size / (1024 * 1024)
    signature = "signed and verified" if should_sign else "unsigned"
    print(f"Built {destination} ({size_mib:.1f} MiB, {signature})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
