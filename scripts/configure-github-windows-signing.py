#!/usr/bin/env python3
"""Upload a Windows signing PFX and password to GitHub Actions secrets."""

from __future__ import annotations

import argparse
import base64
import getpass
import shutil
import subprocess
import sys
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Configure DeskLink Windows release signing secrets on GitHub."
    )
    parser.add_argument("pfx", type=Path, help="trusted code-signing PFX file")
    parser.add_argument(
        "--repo",
        help="optional OWNER/REPO target; defaults to the current gh repository",
    )
    return parser.parse_args()


def set_secret(name: str, value: str, repository: str | None) -> None:
    command = ["gh", "secret", "set", name]
    if repository:
        command.extend(["--repo", repository])
    subprocess.run(command, input=value, text=True, check=True)


def main() -> int:
    arguments = parse_args()
    if not shutil.which("gh"):
        raise SystemExit("GitHub CLI (gh) is required.")

    pfx = arguments.pfx.expanduser().resolve()
    if not pfx.is_file():
        raise SystemExit(f"PFX does not exist: {pfx}")
    password = getpass.getpass("请输入 PFX 密码（不会显示或写入磁盘）：")
    if not password:
        raise SystemExit("PFX password cannot be empty.")

    encoded = base64.b64encode(pfx.read_bytes()).decode("ascii")
    set_secret("WINDOWS_SIGNING_PFX_BASE64", encoded, arguments.repo)
    set_secret("WINDOWS_SIGNING_PFX_PASSWORD", password, arguments.repo)
    print("GitHub Windows signing secrets configured successfully.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
