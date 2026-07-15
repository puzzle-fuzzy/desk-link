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
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WINDOWS_UI = ROOT / "apps" / "windows-ui"
TARGET = "x86_64-pc-windows-msvc"
PRODUCT_CONFIG = WINDOWS_UI / "src" / "product-config.ts"
RUST_RELAY_CONFIG = WINDOWS_UI / "src-tauri" / "src" / "local_relay.rs"


def run(command: list[str], *, cwd: Path = ROOT) -> None:
    print("+", subprocess.list2cmdline(command), flush=True)
    subprocess.run(command, cwd=cwd, check=True)


def cargo_version(path: str) -> str:
    manifest = tomllib.loads((ROOT / path).read_text(encoding="utf-8"))
    return str(manifest["package"]["version"])


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
    version = verify_versions()
    managed_relay = verify_managed_relay_profile()
    run([sys.executable, str(ROOT / "scripts" / "generate-windows-assets.py")])
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
        "custom_protocol": True,
        "frontend_assets": assets,
        "managed_relay": managed_relay,
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
