#!/usr/bin/env python3
"""Deploy a verified DeskLink relay image over SSH with automatic rollback."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shlex
import subprocess
import time
from pathlib import Path, PurePosixPath


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_ARCHIVE = ROOT / "dist" / "linux" / "desklink-relay-0.1.0-amd64.tar"
SAFE_LABEL = re.compile(r"^[A-Za-z0-9_./:@+-]+$")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--target",
        default=os.environ.get("DESKLINK_RELAY_SSH_TARGET", ""),
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
    parser.add_argument("--archive", type=Path, default=DEFAULT_ARCHIVE)
    parser.add_argument("--container", default="desklink-relay-relay-1")
    parser.add_argument("--health-timeout", type=int, default=45)
    return parser.parse_args()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def validate_remote_value(label: str, value: str, *, absolute: bool = False) -> str:
    value = value.strip()
    if not value or not SAFE_LABEL.fullmatch(value):
        raise RuntimeError(f"unsafe or missing {label}: {value!r}")
    if absolute and not PurePosixPath(value).is_absolute():
        raise RuntimeError(f"{label} must be an absolute path: {value!r}")
    return value


class Ssh:
    def __init__(self, target: str, identity_file: Path | None) -> None:
        self.target = validate_remote_value("SSH target", target)
        self.base = ["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]
        if identity_file:
            self.base.extend(["-i", str(identity_file)])

    def run(self, arguments: list[str], *, check: bool = True) -> str:
        command = shlex.join(arguments)
        completed = subprocess.run(
            [*self.base, self.target, command],
            check=False,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=60,
        )
        if check and completed.returncode != 0:
            raise RuntimeError(
                f"remote command failed ({command}): {completed.stderr.strip()[-800:]}"
            )
        return completed.stdout.strip()

    def copy(self, source: Path, destination: str) -> None:
        destination = validate_remote_value("remote archive", destination, absolute=True)
        with source.open("rb") as payload:
            completed = subprocess.run(
                [
                    *self.base,
                    self.target,
                    shlex.join(["dd", f"of={destination}", "status=none"]),
                ],
                stdin=payload,
                check=False,
                capture_output=True,
                timeout=120,
            )
        if completed.returncode != 0:
            detail = completed.stderr.decode("utf-8", errors="replace").strip()[-800:]
            raise RuntimeError(f"archive upload failed: {detail}")


def compose_command(
    workdir: str, config_file: str, service: str
) -> list[str]:
    return [
        "docker",
        "compose",
        "--project-directory",
        workdir,
        "--file",
        config_file,
        "up",
        "--detach",
        "--no-deps",
        "--force-recreate",
        service,
    ]


def healthy(ssh: Ssh, container: str) -> bool:
    state = json.loads(
        ssh.run(
            ["docker", "inspect", container, "--format", "{{json .State}}"],
            check=False,
        )
        or "{}"
    )
    return state.get("Running") is True and state.get("Health", {}).get("Status") == "healthy"


def main() -> int:
    arguments = parse_args()
    if not arguments.target:
        raise SystemExit("--target or DESKLINK_RELAY_SSH_TARGET is required")
    if arguments.identity_file and not arguments.identity_file.is_file():
        raise SystemExit(f"SSH identity does not exist: {arguments.identity_file}")
    if not arguments.archive.is_file():
        raise SystemExit(f"relay archive does not exist: {arguments.archive}")
    container = validate_remote_value("container", arguments.container)
    archive_sha256 = sha256(arguments.archive)
    remote_archive = f"/tmp/desklink-relay-{archive_sha256[:16]}.tar"
    ssh = Ssh(arguments.target, arguments.identity_file)

    label = lambda name: ssh.run(
        ["docker", "inspect", container, "--format", f'{{{{index .Config.Labels "{name}"}}}}']
    )
    workdir = validate_remote_value(
        "compose working directory",
        label("com.docker.compose.project.working_dir"),
        absolute=True,
    )
    config_file = validate_remote_value(
        "compose config file",
        label("com.docker.compose.project.config_files").split(",", 1)[0],
        absolute=True,
    )
    service = validate_remote_value("compose service", label("com.docker.compose.service"))
    old_image = validate_remote_value(
        "current image ID",
        ssh.run(["docker", "inspect", container, "--format", "{{.Image}}"]),
    )
    rollback_tag = f"desklink-relay:rollback-{int(time.time())}"
    deploy = compose_command(workdir, config_file, service)

    ssh.copy(arguments.archive, remote_archive)
    try:
        remote_sha256 = ssh.run(["sha256sum", remote_archive]).split()[0]
        if remote_sha256 != archive_sha256:
            raise RuntimeError("uploaded relay archive checksum does not match")
        ssh.run(["docker", "tag", old_image, rollback_tag])
        ssh.run(["docker", "load", "--input", remote_archive])
        ssh.run(deploy)

        deadline = time.monotonic() + max(5, arguments.health_timeout)
        while time.monotonic() < deadline:
            if healthy(ssh, container):
                break
            time.sleep(2)
        else:
            ssh.run(["docker", "tag", rollback_tag, "desklink-relay:0.1.0"])
            ssh.run(deploy)
            raise RuntimeError(f"new relay did not become healthy; restored {rollback_tag}")
    finally:
        ssh.run(["rm", "-f", "--", remote_archive], check=False)

    new_image = ssh.run(["docker", "inspect", container, "--format", "{{.Image}}"])
    result = {
        "container": container,
        "previous_image_id": old_image,
        "deployed_image_id": new_image,
        "rollback_tag": rollback_tag,
        "archive_sha256": archive_sha256,
        "healthy": True,
    }
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        raise SystemExit(f"relay deployment failed: {error}") from error
