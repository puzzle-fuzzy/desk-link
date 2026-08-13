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
PROBE_TIMEOUT_SECONDS = 90
PROBE_MAX_ATTEMPTS = 3
PROBE_BACKOFF_SECONDS = 3


def managed_profile() -> tuple[str, str]:
    source = PRODUCT_CONFIG.read_text(encoding="utf-8")
    address = re.search(r'MANAGED_RELAY_ADDRESS\s*=\s*"([^"]+)"', source)
    server_name = re.search(r'MANAGED_RELAY_SERVER_NAME\s*=\s*"([^"]+)"', source)
    if not address or not server_name:
        raise SystemExit("Managed relay profile could not be read from product-config.ts")
    return address.group(1), server_name.group(1)


def output_text(value: object) -> str:
    """Normalize subprocess output, including bytes from a timeout exception."""
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return str(value)


def run_probe(command: list[str]) -> tuple[int, str, int, int]:
    """Run the public probe with bounded retries for transient runner/network loss."""
    started = time.monotonic()
    attempts = 0
    exit_code = 124
    outputs: list[str] = []

    for attempt in range(1, PROBE_MAX_ATTEMPTS + 1):
        attempts = attempt
        try:
            completed = subprocess.run(
                command,
                cwd=ROOT,
                check=False,
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=PROBE_TIMEOUT_SECONDS,
            )
            exit_code = completed.returncode
            attempt_output = (
                output_text(completed.stdout) + output_text(completed.stderr)
            ).strip()
            outputs.append(f"attempt {attempt}: exit_code={exit_code}\n{attempt_output}")
            if exit_code == 0:
                break
        except subprocess.TimeoutExpired as error:
            exit_code = 124
            attempt_output = (
                output_text(error.stdout) + output_text(error.stderr)
            ).strip()
            outputs.append(
                f"attempt {attempt}: timeout={PROBE_TIMEOUT_SECONDS}s\n{attempt_output}"
            )

        if attempt < PROBE_MAX_ATTEMPTS:
            delay = PROBE_BACKOFF_SECONDS * attempt
            print(
                f"Probe attempt {attempt} failed; retrying in {delay}s",
                flush=True,
            )
            time.sleep(delay)

    elapsed_ms = max(1, round((time.monotonic() - started) * 1000))
    return exit_code, "\n".join(outputs).strip()[-2_000:], attempts, elapsed_ms


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
    exit_code, output, attempts, elapsed_ms = run_probe(command)
    report = {
        "schema": 1,
        "relay_address": address,
        "tls_server_name": server_name,
        "elapsed_ms": elapsed_ms,
        "exit_code": exit_code,
        "passed": exit_code == 0,
        "attempts": attempts,
        "output": output,
        "completed_at_unix_s": int(time.time()),
    }
    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    if exit_code != 0:
        raise SystemExit(f"Managed relay verification failed; report: {REPORT}")
    print(output)
    print(f"Report: {REPORT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
