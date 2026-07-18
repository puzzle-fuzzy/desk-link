#!/usr/bin/env python3
"""Install one immutable DeskLink diagnostics release on the relay server."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import pwd
import shutil
import subprocess
import tarfile
import urllib.request


RELEASES = Path("/opt/desklink-diagnostics/releases")
CURRENT = Path("/opt/desklink-diagnostics/current")
BIN_DIRECTORY = Path("/opt/desklink-diagnostics/bin")
BUN_EXECUTABLE = BIN_DIRECTORY / "bun"
STATE = Path("/var/lib/desklink-diagnostics")
SERVICE = Path("/etc/systemd/system/desklink-diagnostics.service")
NGINX_SITE = Path("/etc/nginx/conf.d/p2p.yxswy.com.conf")
NGINX_RATE = Path("/etc/nginx/conf.d/desklink-diagnostics-rate.conf")
BEGIN_MARKER = "    # BEGIN DESKLINK DIAGNOSTICS"
END_MARKER = "    # END DESKLINK DIAGNOSTICS"
ANCHOR = "    location ^~ /assets/ {"


def run(arguments: list[str]) -> None:
    subprocess.run(arguments, check=True)


def ensure_user() -> None:
    try:
        pwd.getpwnam("desklink-diagnostics")
    except KeyError:
        run(
            [
                "useradd",
                "--system",
                "--home-dir",
                str(STATE),
                "--shell",
                "/sbin/nologin",
                "desklink-diagnostics",
            ]
        )


def install_bun_runtime() -> None:
    source = Path(os.path.realpath("/usr/local/bin/bun"))
    if not source.is_file():
        raise RuntimeError("server Bun runtime was not found")
    BIN_DIRECTORY.mkdir(parents=True, exist_ok=True)
    temporary = BUN_EXECUTABLE.with_suffix(".tmp")
    shutil.copy2(source, temporary)
    os.chmod(temporary, 0o755)
    os.replace(temporary, BUN_EXECUTABLE)


def safe_extract(archive: Path, destination: Path) -> None:
    destination.mkdir(parents=True, exist_ok=False)
    root = destination.resolve()
    with tarfile.open(archive, "r:gz") as bundle:
        for member in bundle.getmembers():
            target = (destination / member.name).resolve()
            if target != root and root not in target.parents:
                raise RuntimeError("diagnostics archive contains an unsafe path")
            if member.issym() or member.islnk():
                raise RuntimeError("diagnostics archive must not contain links")
        bundle.extractall(destination)


def nginx_configuration(release: Path) -> tuple[str, str]:
    snippet = (release / "deploy/nginx-location.conf").read_text(encoding="utf-8")
    lines = snippet.splitlines()
    if not lines or not lines[0].startswith("limit_req_zone "):
        raise RuntimeError("diagnostics nginx rate limit is missing")
    rate = lines[0] + "\n"
    locations = "\n".join(lines[1:]).strip()
    indented = "\n".join(f"    {line}" if line else "" for line in locations.splitlines())
    block = f"{BEGIN_MARKER}\n{indented}\n{END_MARKER}\n\n"
    site = NGINX_SITE.read_text(encoding="utf-8")
    if BEGIN_MARKER in site and END_MARKER in site:
        before, remaining = site.split(BEGIN_MARKER, 1)
        _, after = remaining.split(END_MARKER, 1)
        after = after.lstrip("\n")
        site = f"{before}{block}{after}"
    else:
        if ANCHOR not in site:
            raise RuntimeError("p2p nginx HTTPS server anchor was not found")
        site = site.replace(ANCHOR, block + ANCHOR, 1)
    return rate, site


def install(archive: Path, release_id: str) -> dict[str, str]:
    if not release_id or any(character not in "0123456789abcdef" for character in release_id):
        raise RuntimeError("release identifier must be lowercase hexadecimal")
    release = RELEASES / release_id
    RELEASES.mkdir(parents=True, exist_ok=True)
    if not release.exists():
        safe_extract(archive, release)
    required = [
        release / "package.json",
        release / "src/index.ts",
        release / "deploy/desklink-diagnostics.service",
        release / "deploy/nginx-location.conf",
    ]
    if any(not path.is_file() for path in required):
        raise RuntimeError("diagnostics release is incomplete")

    old_site = NGINX_SITE.read_text(encoding="utf-8")
    old_rate = NGINX_RATE.read_text(encoding="utf-8") if NGINX_RATE.exists() else None
    rate, site = nginx_configuration(release)
    NGINX_RATE.write_text(rate, encoding="utf-8")
    NGINX_SITE.write_text(site, encoding="utf-8")
    try:
        run(["nginx", "-t"])
    except Exception:
        NGINX_SITE.write_text(old_site, encoding="utf-8")
        if old_rate is None:
            NGINX_RATE.unlink(missing_ok=True)
        else:
            NGINX_RATE.write_text(old_rate, encoding="utf-8")
        raise

    ensure_user()
    install_bun_runtime()
    STATE.mkdir(parents=True, exist_ok=True)
    account = pwd.getpwnam("desklink-diagnostics")
    os.chown(STATE, account.pw_uid, account.pw_gid)
    os.chmod(STATE, 0o750)
    for path in release.rglob("*"):
        if path.is_file():
            os.chmod(path, 0o644)
    os.chmod(release, 0o755)

    temporary_link = CURRENT.with_name("current.next")
    temporary_link.unlink(missing_ok=True)
    temporary_link.symlink_to(release)
    os.replace(temporary_link, CURRENT)
    shutil.copy2(release / "deploy/desklink-diagnostics.service", SERVICE)
    run(["systemctl", "daemon-reload"])
    run(["systemctl", "enable", "desklink-diagnostics.service"])
    run(["systemctl", "restart", "desklink-diagnostics.service"])
    run(["systemctl", "is-active", "--quiet", "desklink-diagnostics.service"])
    run(["nginx", "-s", "reload"])
    with urllib.request.urlopen("http://127.0.0.1:3411/health", timeout=5) as response:
        if response.status != 200:
            raise RuntimeError("diagnostics health check failed")
    archive.unlink(missing_ok=True)
    return {"release": release_id, "service": "active", "health": "ok"}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("archive", type=Path)
    parser.add_argument("release_id")
    arguments = parser.parse_args()
    print(json.dumps(install(arguments.archive, arguments.release_id), sort_keys=True))


if __name__ == "__main__":
    main()
