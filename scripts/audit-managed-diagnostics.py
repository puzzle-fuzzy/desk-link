#!/usr/bin/env python3
"""Audit managed diagnostics infrastructure without reading session details."""

from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
import time
import urllib.request
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_REPORT = ROOT / "dist" / "linux" / "managed-diagnostics-audit.json"
PUBLIC_HEALTH = "https://p2p.yxswy.com/desklink-diagnostics/health"
REMOTE_REPORT = "/var/lib/desklink-diagnostics/health-report.json"
REMOTE_SUMMARY_SCRIPT = f"""
import json
report = json.load(open({REMOTE_REPORT!r}, encoding='utf-8'))
print(json.dumps({{
    'schema': report.get('schema'),
    'status': report.get('status'),
    'requires_attention': report.get('requires_attention'),
    'generated_at': report.get('generated_at'),
    'window_hours': report.get('window_hours'),
    'summary': report.get('summary'),
}}, separators=(',', ':')))
""".strip()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--target",
        default=os.environ.get(
            "DESKLINK_DIAGNOSTICS_SSH_TARGET", "root@101.35.246.159"
        ),
    )
    parser.add_argument(
        "--identity-file",
        type=Path,
        default=(
            Path(os.environ["DESKLINK_DIAGNOSTICS_SSH_IDENTITY"])
            if os.environ.get("DESKLINK_DIAGNOSTICS_SSH_IDENTITY")
            else Path.home() / ".ssh" / "p2p-tencent-ed25519"
        ),
    )
    parser.add_argument("--report", type=Path, default=DEFAULT_REPORT)
    return parser.parse_args()


def remote(arguments: argparse.Namespace, command: list[str]) -> str:
    completed = subprocess.run(
        [
            "ssh",
            "-i",
            str(arguments.identity_file),
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            arguments.target,
            shlex.join(command),
        ],
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=20,
    )
    if completed.returncode != 0:
        raise RuntimeError(completed.stderr.strip()[-800:])
    return completed.stdout.strip()


def validate_summary(value: object, now: datetime | None = None) -> dict[str, object]:
    if not isinstance(value, dict) or value.get("schema") != 1:
        raise ValueError("diagnostics health report schema is invalid")
    if value.get("status") not in {"healthy", "attention", "empty"}:
        raise ValueError("diagnostics health report status is invalid")
    if not isinstance(value.get("requires_attention"), bool):
        raise ValueError("diagnostics attention state is invalid")
    generated = value.get("generated_at")
    if not isinstance(generated, str):
        raise ValueError("diagnostics report time is missing")
    generated_at = datetime.fromisoformat(generated.replace("Z", "+00:00"))
    age_seconds = ((now or datetime.now(timezone.utc)) - generated_at).total_seconds()
    if age_seconds < -600 or age_seconds > 2 * 60 * 60:
        raise ValueError("diagnostics health report is stale")
    summary = value.get("summary")
    if not isinstance(summary, dict):
        raise ValueError("diagnostics aggregate summary is missing")
    for key in ("sessions", "healthy", "warning", "error", "incomplete"):
        field = summary.get(key)
        if not isinstance(field, int) or isinstance(field, bool) or field < 0:
            raise ValueError(f"diagnostics aggregate {key} is invalid")
    return value


def main() -> int:
    arguments = parse_args()
    if not arguments.identity_file.is_file():
        raise SystemExit(f"SSH identity does not exist: {arguments.identity_file}")
    with urllib.request.urlopen(PUBLIC_HEALTH, timeout=10) as response:
        public_health = json.load(response)
        public_ok = response.status == 200 and public_health.get("status") == "ok"
    service = remote(
        arguments, ["systemctl", "is-active", "desklink-diagnostics.service"]
    )
    timer = remote(
        arguments,
        ["systemctl", "is-active", "desklink-diagnostics-analysis.timer"],
    )
    aggregate = validate_summary(
        json.loads(remote(arguments, ["python3", "-c", REMOTE_SUMMARY_SCRIPT]))
    )
    checks = {
        "public_health": public_ok,
        "service_active": service == "active",
        "analysis_timer_active": timer == "active",
        "report_fresh": True,
    }
    report = {
        "schema": 1,
        "checks": checks,
        "aggregate": aggregate,
        "passed": all(checks.values()),
        "completed_at_unix_s": int(time.time()),
    }
    arguments.report.parent.mkdir(parents=True, exist_ok=True)
    arguments.report.write_text(
        json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, ValueError, RuntimeError, json.JSONDecodeError) as error:
        raise SystemExit(f"managed diagnostics audit failed: {error}") from error
