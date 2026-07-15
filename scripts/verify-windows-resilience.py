#!/usr/bin/env python3
"""Run repeatable Windows host resilience acceptance checks."""

from __future__ import annotations

import argparse
import ctypes
import json
import os
import subprocess
import sys
import time
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_REPORT = ROOT / "dist" / "windows" / "windows-resilience-report.json"


def display_topology() -> dict[str, object]:
    user32 = ctypes.windll.user32
    metrics = {
        "monitor_count": 80,
        "primary_width": 0,
        "primary_height": 1,
        "virtual_left": 76,
        "virtual_top": 77,
        "virtual_width": 78,
        "virtual_height": 79,
    }
    values = {name: int(user32.GetSystemMetrics(index)) for name, index in metrics.items()}
    return {
        "monitor_count": values["monitor_count"],
        "primary": {
            "left": 0,
            "top": 0,
            "width": values["primary_width"],
            "height": values["primary_height"],
        },
        "virtual_desktop": {
            "left": values["virtual_left"],
            "top": values["virtual_top"],
            "width": values["virtual_width"],
            "height": values["virtual_height"],
        },
    }


def run_check(command: list[str], environment: dict[str, str]) -> dict[str, object]:
    started = time.monotonic()
    print(f"+ {' '.join(command)}", flush=True)
    result = subprocess.run(command, cwd=ROOT, env=environment, check=False)
    record = {
        "command": command,
        "elapsed_seconds": round(time.monotonic() - started, 3),
        "exit_code": result.returncode,
    }
    if result.returncode != 0:
        raise AcceptanceFailure(record)
    return record


class AcceptanceFailure(RuntimeError):
    def __init__(self, record: dict[str, object]) -> None:
        super().__init__(f"acceptance command failed with exit code {record['exit_code']}")
        self.record = record


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--soak-seconds",
        type=int,
        default=30,
        help="Continuous encrypted media soak duration (10-300 seconds; default: 30).",
    )
    parser.add_argument(
        "--report",
        type=Path,
        default=DEFAULT_REPORT,
        help=f"JSON report path (default: {DEFAULT_REPORT}).",
    )
    return parser.parse_args()


def main() -> int:
    if sys.platform != "win32":
        raise SystemExit("Windows resilience verification must run on Windows")
    args = parse_args()
    if not 10 <= args.soak_seconds <= 300:
        raise SystemExit("--soak-seconds must be between 10 and 300")
    sys.stdout.reconfigure(encoding="utf-8")
    environment = os.environ.copy()
    environment["DESKLINK_SOAK_SECONDS"] = str(args.soak_seconds)
    report: dict[str, object] = {
        "schema": 1,
        "platform": sys.platform,
        "soak_seconds": args.soak_seconds,
        "display_topology": display_topology(),
        "checks": [],
        "passed": False,
    }
    commands = [
        [
            "cargo",
            "test",
            "-p",
            "desklink-windows",
            "--test",
            "capture_smoke",
            "--",
            "--nocapture",
        ],
        [
            "cargo",
            "test",
            "-p",
            "desklink-windows",
            "--test",
            "host_supervisor",
            "repeated_relay_restarts_rebuild_host_runtime_with_fresh_streams",
            "--",
            "--nocapture",
        ],
        [
            "cargo",
            "test",
            "-p",
            "desklink-windows-ui",
            "power::tests",
            "--lib",
        ],
        [
            "cargo",
            "test",
            "-p",
            "desklink-windows",
            "--test",
            "runtime_smoke",
            "local_relay_hardware_soak_keeps_secure_media_and_cursor_alive",
            "--",
            "--ignored",
            "--nocapture",
        ],
    ]
    try:
        for command in commands:
            report["checks"].append(run_check(command, environment))
        report["passed"] = True
    except AcceptanceFailure as error:
        report["checks"].append(error.record)
        report["failure"] = str(error)
    finally:
        report["completed_at_unix_s"] = int(time.time())
        report_path = args.report.resolve()
        report_path.parent.mkdir(parents=True, exist_ok=True)
        report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(f"Report: {report_path}")
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
