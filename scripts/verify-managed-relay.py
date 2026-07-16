#!/usr/bin/env python3
"""Verify the managed DeskLink relay with the production QUIC/TLS client."""

from __future__ import annotations

import json
import re
import subprocess
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PRODUCT_CONFIG = ROOT / "apps" / "windows-ui" / "src" / "product-config.ts"
REPORT = ROOT / "dist" / "windows" / "managed-relay-verification.json"


def managed_profile() -> tuple[str, str]:
    source = PRODUCT_CONFIG.read_text(encoding="utf-8")
    address = re.search(r'MANAGED_RELAY_ADDRESS\s*=\s*"([^"]+)"', source)
    server_name = re.search(r'MANAGED_RELAY_SERVER_NAME\s*=\s*"([^"]+)"', source)
    if not address or not server_name:
        raise SystemExit("Managed relay profile could not be read from product-config.ts")
    return address.group(1), server_name.group(1)


def main() -> int:
    address, server_name = managed_profile()
    command = [
        "cargo",
        "run",
        "--locked",
        "--quiet",
        "-p",
        "desklink-transport",
        "--example",
        "directory_probe",
        "--",
        address,
        server_name,
    ]
    print("+", subprocess.list2cmdline(command), flush=True)
    started = time.monotonic()
    completed = subprocess.run(
        command,
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=90,
    )
    elapsed_ms = max(1, round((time.monotonic() - started) * 1000))
    report = {
        "schema": 1,
        "relay_address": address,
        "tls_server_name": server_name,
        "elapsed_ms": elapsed_ms,
        "exit_code": completed.returncode,
        "passed": completed.returncode == 0,
        "output": (completed.stdout + completed.stderr).strip()[-2_000:],
        "completed_at_unix_s": int(time.time()),
    }
    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    if completed.returncode != 0:
        raise SystemExit(f"Managed relay verification failed; report: {REPORT}")
    print(completed.stdout.strip())
    print(f"Report: {REPORT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
