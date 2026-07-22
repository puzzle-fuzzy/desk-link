#!/usr/bin/env python3
"""Repair the managed diagnostics Nginx locations through an idempotent SSH change."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import textwrap
import time


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_IDENTITY = Path.home() / ".ssh" / "p2p-tencent-ed25519"

REMOTE_SCRIPT = textwrap.dedent(
    r"""
    from pathlib import Path
    import json
    import shutil
    import subprocess
    import time
    import urllib.request

    site = Path("/etc/nginx/conf.d/p2p.yxswy.com.conf")
    rate = Path("/etc/nginx/conf.d/desklink-diagnostics-rate.conf")
    snippet = Path(
        "/opt/desklink-diagnostics/current/deploy/nginx-location.conf"
    ).read_text(encoding="utf-8")
    lines = snippet.splitlines()
    if not lines or not lines[0].startswith("limit_req_zone "):
        raise SystemExit("diagnostics rate limit definition is missing")
    original_site = site.read_text(encoding="utf-8")
    original_rate = rate.read_text(encoding="utf-8") if rate.exists() else None
    begin = "    # BEGIN DESKLINK DIAGNOSTICS"
    end = "    # END DESKLINK DIAGNOSTICS"
    anchor = "    location ^~ /assets/ {"
    body = "\n".join(
        ("    " + line) if line else "" for line in lines[1:]
    ).strip()
    block = f"{begin}\n{body}\n{end}\n\n"
    if begin in original_site and end in original_site:
        before, remaining = original_site.split(begin, 1)
        _, after = remaining.split(end, 1)
        updated_site = before + block + after.lstrip("\n")
    elif anchor in original_site:
        updated_site = original_site.replace(anchor, block + anchor, 1)
    else:
        raise SystemExit("p2p HTTPS server anchor is missing")

    stamp = int(time.time())
    site_backup = site.with_name(site.name + f".bak-desklink-{stamp}")
    rate_backup = rate.with_name(rate.name + f".bak-desklink-{stamp}")
    shutil.copy2(site, site_backup)
    if rate.exists():
        shutil.copy2(rate, rate_backup)
    try:
        rate.write_text(lines[0] + "\n", encoding="utf-8")
        site.write_text(updated_site, encoding="utf-8")
        subprocess.run(["nginx", "-t"], check=True)
        subprocess.run(["nginx", "-s", "reload"], check=True)
        loaded = subprocess.run(
            ["nginx", "-T"],
            check=True,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
        loaded_text = loaded.stdout + loaded.stderr
        if begin not in loaded_text or end not in loaded_text:
            raise RuntimeError("nginx -T did not load diagnostics locations")
        deadline = time.monotonic() + 20
        consecutive_ok = 0
        last_status = "unknown"
        last_payload = ""
        while time.monotonic() < deadline:
            try:
                with urllib.request.urlopen(
                    "https://p2p.yxswy.com/desklink-diagnostics/health", timeout=5
                ) as response:
                    last_status = str(response.status)
                    last_payload = response.read().decode("utf-8")
                    parsed = json.loads(last_payload)
                    if response.status == 200 and parsed.get("status") == "ok":
                        consecutive_ok += 1
                    else:
                        consecutive_ok = 0
            except Exception as error:
                last_status = str(error)
                consecutive_ok = 0
            if consecutive_ok >= 3:
                print(f"PUBLIC {last_status} {last_payload}")
                break
            time.sleep(1)
        else:
            raise RuntimeError(
                f"public diagnostics health did not stabilize: {last_status} {last_payload}"
            )
    except Exception:
        shutil.copy2(site_backup, site)
        if original_rate is None:
            rate.unlink(missing_ok=True)
        else:
            rate.write_text(original_rate, encoding="utf-8")
        subprocess.run(["nginx", "-t"], check=True)
        raise
    print(f"BACKUP {site_backup}")
    """
).strip()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default="root@101.35.246.159")
    parser.add_argument("--identity", type=Path, default=DEFAULT_IDENTITY)
    return parser.parse_args()


def main() -> int:
    arguments = parse_args()
    if not arguments.identity.is_file():
        raise SystemExit(f"SSH identity does not exist: {arguments.identity}")
    command = [
        "ssh",
        "-i",
        str(arguments.identity),
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        arguments.host,
        "python3",
        "-",
    ]
    completed = subprocess.run(
        command,
        cwd=ROOT,
        input=REMOTE_SCRIPT + "\n",
        text=True,
        encoding="utf-8",
        errors="replace",
        capture_output=True,
        check=False,
        timeout=60,
    )
    if completed.stdout:
        print(completed.stdout, end="")
    if completed.returncode != 0:
        if completed.stderr:
            print(completed.stderr, end="")
        return completed.returncode
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
