#!/usr/bin/env python3
"""Audit the managed relay host without collecting session or user data."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import time
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_REPORT = ROOT / "dist" / "linux" / "managed-relay-host-audit.json"
CAPACITY_PATTERN = re.compile(
    r"relay_capacity active_sessions=(\d+) attached_participants=(\d+) "
    r"accepted_connections=(\d+) max_sessions=(\d+) max_connections=(\d+)"
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--target",
        default=os.environ.get("DESKLINK_RELAY_SSH_TARGET", ""),
        help="SSH target, for example root@relay.example.com",
    )
    parser.add_argument(
        "--identity-file",
        type=Path,
        default=(
            Path(os.environ["DESKLINK_RELAY_SSH_IDENTITY"])
            if os.environ.get("DESKLINK_RELAY_SSH_IDENTITY")
            else None
        ),
    )
    parser.add_argument("--container", default="desklink-relay-relay-1")
    parser.add_argument(
        "--certificate",
        default="/etc/letsencrypt/live/p2p.yxswy.com/fullchain.pem",
    )
    parser.add_argument("--minimum-certificate-days", type=int, default=21)
    parser.add_argument("--maximum-disk-percent", type=int, default=80)
    parser.add_argument("--report", type=Path, default=DEFAULT_REPORT)
    return parser.parse_args()


def ssh_command(arguments: argparse.Namespace, remote_command: str) -> list[str]:
    command = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]
    if arguments.identity_file:
        command.extend(["-i", str(arguments.identity_file)])
    command.extend([arguments.target, remote_command])
    return command


def remote(
    arguments: argparse.Namespace, command: str, *, include_stderr: bool = False
) -> str:
    completed = subprocess.run(
        ssh_command(arguments, command),
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=20,
    )
    if completed.returncode != 0:
        detail = completed.stderr.strip()[-500:]
        raise RuntimeError(f"remote audit command failed ({command}): {detail}")
    output = completed.stdout
    if include_stderr:
        output += completed.stderr
    return output.strip()


def parse_certificate_expiry(value: str, now: datetime | None = None) -> int:
    text = value.strip().removeprefix("notAfter=")
    expires = datetime.strptime(text, "%b %d %H:%M:%S %Y %Z").replace(
        tzinfo=timezone.utc
    )
    current = now or datetime.now(timezone.utc)
    return int((expires - current).total_seconds() // 86_400)


def parse_disk_percent(output: str) -> int:
    lines = [line for line in output.splitlines() if line.strip()]
    if len(lines) < 2:
        raise ValueError("df output is incomplete")
    return int(lines[-1].split()[4].rstrip("%"))


def parse_capacity(logs: str) -> dict[str, int] | None:
    matches = list(CAPACITY_PATTERN.finditer(logs))
    if not matches:
        return None
    values = [int(value) for value in matches[-1].groups()]
    return dict(
        zip(
            (
                "active_sessions",
                "attached_participants",
                "accepted_connections",
                "max_sessions",
                "max_connections",
            ),
            values,
            strict=True,
        )
    )


def main() -> int:
    arguments = parse_args()
    if not arguments.target:
        raise SystemExit("--target or DESKLINK_RELAY_SSH_TARGET is required")
    if arguments.identity_file and not arguments.identity_file.is_file():
        raise SystemExit(f"SSH identity does not exist: {arguments.identity_file}")

    state = json.loads(
        remote(
            arguments,
            f'docker inspect {arguments.container} --format "{{{{json .State}}}}"',
        )
    )
    restart_policy = json.loads(
        remote(
            arguments,
            f'docker inspect {arguments.container} --format "{{{{json .HostConfig.RestartPolicy}}}}"',
        )
    )
    restart_count = int(
        remote(
            arguments,
            f'docker inspect {arguments.container} --format "{{{{.RestartCount}}}}"',
        )
    )
    image = remote(
        arguments,
        f'docker inspect {arguments.container} --format "{{{{.Config.Image}}}}"',
    )
    certificate_days = parse_certificate_expiry(
        remote(arguments, f"openssl x509 -in {arguments.certificate} -noout -enddate")
    )
    disk_percent = parse_disk_percent(remote(arguments, "df -P /"))
    logs = remote(
        arguments,
        f"docker logs --since 5m {arguments.container}",
        include_stderr=True,
    )
    capacity = parse_capacity(logs)

    checks = {
        "container_running": state.get("Running") is True,
        "container_healthy": state.get("Health", {}).get("Status") == "healthy",
        "restart_policy": restart_policy.get("Name") in {"always", "unless-stopped"},
        "certificate_window": certificate_days >= arguments.minimum_certificate_days,
        "disk_capacity": disk_percent <= arguments.maximum_disk_percent,
        "capacity_sample": capacity is not None,
    }
    if capacity:
        checks["session_capacity"] = (
            capacity["active_sessions"] * 100 < capacity["max_sessions"] * 80
        )
        checks["connection_capacity"] = (
            capacity["accepted_connections"] * 100
            < capacity["max_connections"] * 80
        )

    report = {
        "schema": 1,
        "container": arguments.container,
        "image": image,
        "running": state.get("Running") is True,
        "health": state.get("Health", {}).get("Status", "missing"),
        "restart_count": restart_count,
        "restart_policy": restart_policy.get("Name", "missing"),
        "certificate_days_remaining": certificate_days,
        "disk_used_percent": disk_percent,
        "capacity": capacity,
        "checks": checks,
        "passed": all(checks.values()),
        "completed_at_unix_s": int(time.time()),
    }
    arguments.report.parent.mkdir(parents=True, exist_ok=True)
    arguments.report.write_text(
        json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps(report, ensure_ascii=False, indent=2))
    if not report["passed"]:
        raise SystemExit(f"Managed relay host audit failed; report: {arguments.report}")
    print(f"Report: {arguments.report}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, json.JSONDecodeError) as error:
        raise SystemExit(f"Managed relay host audit failed: {error}") from error
