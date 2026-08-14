#!/usr/bin/env python3
"""Verify that the Windows release executable is built from the current UI sources."""

from __future__ import annotations

import hashlib
import json
import os
import re
import struct
import subprocess
import sys
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
WINDOWS_UI = ROOT / "apps" / "windows-ui"
TARGET = "x86_64-pc-windows-msvc"
README = ROOT / "README.md"
COMMIT_SHA = re.compile(r"^[0-9a-fA-F]{40}$")
BUN_VERSION = re.compile(r"^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$")
PRODUCT_CONFIG = WINDOWS_UI / "src" / "product-config.ts"
RUST_RELAY_CONFIG = WINDOWS_UI / "src-tauri" / "src" / "local_relay.rs"
WINDOWS_ASSETS = ROOT / "apps" / "windows" / "assets"
PRODUCTION_RUST_SOURCE_ROOTS = (
    ROOT / "apps" / "windows" / "src",
    ROOT / "apps" / "windows-ui" / "src-tauri" / "src",
)


def run(command: list[str], *, cwd: Path = ROOT) -> None:
    print("+", subprocess.list2cmdline(command), flush=True)
    subprocess.run(command, cwd=cwd, check=True)


def cargo_version(path: str) -> str:
    return package_version(ROOT / path)


def source_metadata() -> tuple[str, bool]:
    """Return the source commit and whether the checkout has local changes."""
    commit = os.environ.get("GITHUB_SHA", "").strip()
    if commit and not COMMIT_SHA.fullmatch(commit):
        raise SystemExit("GITHUB_SHA must contain a 40-character commit SHA")
    if not commit:
        result = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
        )
        commit = result.stdout.strip()
        if result.returncode != 0 or not COMMIT_SHA.fullmatch(commit):
            raise SystemExit("Could not determine the current source commit SHA")
    status = subprocess.run(
        ["git", "status", "--porcelain", "--untracked-files=all"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    if status.returncode != 0:
        raise SystemExit("Could not inspect the source checkout status")
    return commit.lower(), bool(status.stdout.strip())


def verify_versions() -> str:
    versions = {
        "windows-runtime": cargo_version("apps/windows/Cargo.toml"),
        "windows-ui-rust": cargo_version("apps/windows-ui/src-tauri/Cargo.toml"),
        "windows-installer": cargo_version("tools/windows-installer/Cargo.toml"),
        "windows-ui-package": str(
            json.loads((WINDOWS_UI / "package.json").read_text(encoding="utf-8"))["version"]
        ),
        "tauri-config": str(
            json.loads(
                (WINDOWS_UI / "src-tauri" / "tauri.conf.json").read_text(
                    encoding="utf-8"
                )
            )["version"]
        ),
    }
    unique = set(versions.values())
    if len(unique) != 1:
        detail = ", ".join(f"{name}={version}" for name, version in versions.items())
        raise SystemExit(f"Windows release versions do not match: {detail}")
    return unique.pop()


def expected_bun_version() -> str:
    package = json.loads((WINDOWS_UI / "package.json").read_text(encoding="utf-8"))
    package_manager = package.get("packageManager")
    if not isinstance(package_manager, str) or not package_manager.startswith("bun@"):
        raise SystemExit("Windows UI packageManager must pin Bun")
    version = package_manager.removeprefix("bun@").strip()
    if not BUN_VERSION.fullmatch(version):
        raise SystemExit("Windows UI packageManager contains an invalid Bun version")
    return version


def verify_bun_version() -> str:
    expected = expected_bun_version()
    completed = subprocess.run(
        ["bun", "--version"],
        cwd=WINDOWS_UI,
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    actual = completed.stdout.strip()
    if completed.returncode != 0 or actual != expected:
        raise SystemExit(
            f"Bun {expected} is required for the Windows release; detected {actual or 'unavailable'}"
        )
    return actual


def required_match(path: Path, pattern: str, label: str) -> str:
    match = re.search(pattern, path.read_text(encoding="utf-8"))
    if not match:
        raise SystemExit(f"Could not read {label} from {path.relative_to(ROOT)}")
    return match.group(1)


def verify_managed_relay_profile() -> dict[str, str]:
    names = ("MANAGED_RELAY_ADDRESS", "MANAGED_RELAY_SERVER_NAME")
    frontend = {
        name: required_match(
            PRODUCT_CONFIG,
            rf'export\s+const\s+{name}\s*=\s*"([^"]+)"',
            name,
        )
        for name in names
    }
    backend = {
        name: required_match(
            RUST_RELAY_CONFIG,
            rf'pub\s+const\s+{name}:\s*&str\s*=\s*"([^"]+)"',
            name,
        )
        for name in names
    }
    if frontend != backend:
        raise SystemExit(
            "Managed relay profile differs between TypeScript and Rust: "
            f"frontend={frontend}, backend={backend}"
        )
    return {
        "relay_address": frontend["MANAGED_RELAY_ADDRESS"],
        "tls_server_name": frontend["MANAGED_RELAY_SERVER_NAME"],
    }


def verify_release_scope(readme: Path = README) -> dict[str, object]:
    """Keep the public release description aligned with the Windows target."""
    text = readme.read_text(encoding="utf-8")
    required = (
        "Windows 10/11 x64",
        "当前正式发布目标是 Windows 10/11 x64",
        "跨平台研究代码",
    )
    for phrase in required:
        if phrase not in text:
            raise SystemExit(
                f"README release scope is missing the required phrase: {phrase}"
            )
    forbidden = (
        "当前仓库同时包含 Windows 10/11 x64 桌面端与 macOS Apple Silicon 桌面端",
        "## macOS 构建与使用",
        "两台 Apple Silicon Mac 完成",
    )
    for phrase in forbidden:
        if phrase in text:
            raise SystemExit(
                f"README still advertises a non-Windows release path: {phrase}"
            )
    return {
        "target": "windows-10/11-x64",
        "macos_release": False,
        "mobile_release": False,
    }


def verify_static_windows_assets() -> dict[str, dict[str, object]]:
    expected = {
        "desklink-icon.png": b"\x89PNG\r\n\x1a\n",
        "desklink.ico": b"\x00\x00\x01\x00",
    }
    verified: dict[str, dict[str, object]] = {}
    for name, signature in expected.items():
        path = WINDOWS_ASSETS / name
        if not path.is_file():
            raise SystemExit(f"Required Windows asset is missing: {path.relative_to(ROOT)}")
        data = path.read_bytes()
        if len(data) <= len(signature) or not data.startswith(signature):
            raise SystemExit(f"Windows asset has an invalid file signature: {path.relative_to(ROOT)}")
        verified[name] = {
            "size_bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
        }
    return verified


def verify_production_backpressure(
    source_roots: tuple[Path, ...] = PRODUCTION_RUST_SOURCE_ROOTS,
) -> dict[str, object]:
    """Reject unbounded Rust channels in the Windows production data plane."""

    forbidden = (
        ("tokio::sync::mpsc::unbounded_channel", re.compile(r"\bunbounded_channel\s*\(")),
        ("std::sync::mpsc::channel", re.compile(r"std::sync::mpsc::channel\s*\(")),
        # `mpsc::channel()` is also the unbounded std channel when the import
        # is aliased. Bounded Tokio channels always carry an explicit capacity.
        ("unbounded mpsc::channel", re.compile(r"\bmpsc::channel\s*\(\s*\)")),
    )
    checked_files = 0
    violations: list[str] = []
    for source_root in source_roots:
        if not source_root.is_dir():
            raise SystemExit(f"Windows production Rust source directory is missing: {source_root}")
        for path in sorted(source_root.rglob("*.rs")):
            checked_files += 1
            text = path.read_text(encoding="utf-8")
            for label, pattern in forbidden:
                if pattern.search(text):
                    try:
                        display_path = path.relative_to(ROOT)
                    except ValueError:
                        display_path = path
                    violations.append(f"{display_path} ({label})")
    if violations:
        detail = ", ".join(violations)
        raise SystemExit(
            "Windows production Rust contains an unbounded channel; use a bounded "
            f"queue with explicit backpressure: {detail}"
        )
    return {"checked_files": checked_files, "unbounded_channels": 0}


def verify_frontend_assets() -> list[str]:
    index = WINDOWS_UI / "dist" / "index.html"
    if not index.is_file():
        raise SystemExit("Windows UI dist/index.html was not produced")
    text = index.read_text(encoding="utf-8")
    assets = sorted(
        path.relative_to(WINDOWS_UI / "dist").as_posix()
        for path in (WINDOWS_UI / "dist" / "assets").glob("*")
        if path.is_file() and path.suffix in {".css", ".js"}
    )
    if not assets or not any(path.endswith(".js") for path in assets):
        raise SystemExit("Windows UI production assets were not produced")
    for asset in assets:
        if f"/{asset}" not in text:
            raise SystemExit(f"Production index.html does not reference {asset}")
    forbidden_urls = (
        "http://localhost",
        "https://localhost",
        "http://127.0.0.1",
        "https://127.0.0.1",
    )
    for relative_path in ["index.html", *assets]:
        source = (WINDOWS_UI / "dist" / relative_path).read_text(encoding="utf-8")
        found = next((url for url in forbidden_urls if url in source), None)
        if found:
            raise SystemExit(
                f"Production asset {relative_path} contains development URL {found}"
            )
    return assets


def verify_pe(path: Path) -> dict[str, object]:
    data = path.read_bytes()
    if len(data) < 0x100 or data[:2] != b"MZ":
        raise SystemExit(f"Windows release is not a PE executable: {path}")
    pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise SystemExit(f"Windows release has an invalid PE header: {path}")
    machine = struct.unpack_from("<H", data, pe_offset + 4)[0]
    if machine != 0x8664:
        raise SystemExit(f"Windows release is not x64 (machine=0x{machine:04x})")
    digest = hashlib.sha256(data).hexdigest()
    return {"path": str(path), "size_bytes": len(data), "sha256": digest}


def main() -> int:
    if os.name != "nt":
        raise SystemExit("Windows release verification must run on Windows")
    prepare_windows_native_build_environment()
    prepare_windows_release_environment()
    verify_bun_version()
    version = verify_versions()
    source_commit, source_dirty = source_metadata()
    release_scope = verify_release_scope()
    managed_relay = verify_managed_relay_profile()
    windows_assets = verify_static_windows_assets()
    backpressure = verify_production_backpressure()
    run(["bun", "install", "--frozen-lockfile"], cwd=WINDOWS_UI)
    run(["bun", "run", "test"], cwd=WINDOWS_UI)
    run(["bun", "run", "build"], cwd=WINDOWS_UI)
    assets = verify_frontend_assets()
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
    executable = ROOT / "target" / TARGET / "release" / "desklink-windows-ui.exe"
    release = verify_pe(executable)
    report = {
        "schema": 1,
        "version": version,
        "source_commit": source_commit,
        "source_dirty": source_dirty,
        "custom_protocol": True,
        "release_scope": release_scope,
        "frontend_assets": assets,
        "managed_relay": managed_relay,
        "windows_assets": windows_assets,
        "production_backpressure": backpressure,
        "release": release,
        "passed": True,
    }
    report_path = ROOT / "dist" / "windows" / "windows-release-verification.json"
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"Windows release verification passed: {report_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
