#!/usr/bin/env python3
"""Deploy the committed DeskLink diagnostics service through SSH."""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
import subprocess
import tarfile
import tempfile


ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "server" / "diagnostics"


def run(arguments: list[str], *, capture: bool = False) -> str:
    result = subprocess.run(
        arguments,
        cwd=ROOT,
        check=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        capture_output=capture,
    )
    return result.stdout.strip() if capture else ""


def require_clean_commit() -> str:
    run(["git", "diff", "--quiet"])
    run(["git", "diff", "--cached", "--quiet"])
    return run(["git", "rev-parse", "--short=12", "HEAD"], capture=True)


def build_archive(destination: Path) -> None:
    with tarfile.open(destination, "w:gz") as archive:
        for path in sorted(SOURCE.rglob("*")):
            relative = path.relative_to(SOURCE)
            if any(part in {"node_modules", ".git"} for part in relative.parts):
                continue
            if path.is_file():
                archive.add(path, arcname=relative.as_posix(), recursive=False)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def upload(source: Path, host: str, common: list[str], destination: str) -> None:
    with source.open("rb") as payload:
        subprocess.run(
            ["ssh", *common, host, "dd", f"of={destination}", "status=none"],
            cwd=ROOT,
            stdin=payload,
            check=True,
        )
    remote_digest = run(
        ["ssh", *common, host, "sha256sum", destination], capture=True
    ).split()[0]
    if remote_digest != sha256(source):
        raise RuntimeError(f"uploaded file checksum does not match: {source.name}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", default="root@101.35.246.159")
    parser.add_argument(
        "--identity",
        type=Path,
        default=Path.home() / ".ssh" / "p2p-tencent-ed25519",
    )
    arguments = parser.parse_args()
    release_id = require_clean_commit()
    if not arguments.identity.is_file():
        raise SystemExit(f"SSH identity does not exist: {arguments.identity}")
    with tempfile.TemporaryDirectory(prefix="desklink-diagnostics-") as temporary:
        archive = Path(temporary) / f"desklink-diagnostics-{release_id}.tar.gz"
        build_archive(archive)
        remote_archive = f"/tmp/{archive.name}"
        remote_installer = f"/tmp/desklink-diagnostics-install-{release_id}.py"
        common = ["-o", "BatchMode=yes", "-i", str(arguments.identity)]
        upload(archive, arguments.host, common, remote_archive)
        upload(
            SOURCE / "deploy" / "install.py",
            arguments.host,
            common,
            remote_installer,
        )
        run(
            [
                "ssh",
                *common,
                arguments.host,
                "python3",
                remote_installer,
                remote_archive,
                release_id,
            ]
        )


if __name__ == "__main__":
    main()
