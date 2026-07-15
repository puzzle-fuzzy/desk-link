#!/usr/bin/env python3
"""Sign and verify a Windows PE artifact with SignTool.

The signer is selected entirely through environment variables so no private-key
material is stored in the repository or passed on the command line.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


DEFAULT_TIMESTAMP_URL = "http://timestamp.digicert.com"
ARTIFACT_SIGNING_TIMESTAMP_URL = "http://timestamp.acs.microsoft.com"


@dataclass(frozen=True)
class SigningConfiguration:
    mode: str
    dlib: Path | None = None
    metadata: Path | None = None
    thumbprint: str | None = None
    timestamp_url: str = DEFAULT_TIMESTAMP_URL


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Sign and Authenticode-verify a DeskLink Windows artifact."
    )
    parser.add_argument("artifact", type=Path, help="EXE, DLL, MSI, or MSIX to process")
    parser.add_argument(
        "--verify-only",
        action="store_true",
        help="do not sign; only verify the existing Authenticode signature",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="validate configuration and print SignTool commands without running them",
    )
    return parser.parse_args()


def find_signtool() -> Path:
    override = os.environ.get("DESKLINK_SIGNTOOL")
    if override:
        candidate = Path(override).expanduser()
        if candidate.is_file():
            return candidate.resolve()
        raise SystemExit(f"DESKLINK_SIGNTOOL does not exist: {candidate}")

    on_path = shutil.which("signtool.exe") or shutil.which("signtool")
    if on_path:
        return Path(on_path).resolve()

    kits_root = Path(
        os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")
    ) / "Windows Kits" / "10" / "bin"
    candidates = sorted(
        kits_root.glob("*/x64/signtool.exe"),
        key=lambda path: path.parts[-3],
        reverse=True,
    )
    if candidates:
        return candidates[0].resolve()
    raise SystemExit(
        "SignTool was not found. Install the Windows SDK or set DESKLINK_SIGNTOOL."
    )


def read_signing_configuration() -> SigningConfiguration:
    dlib_value = os.environ.get("DESKLINK_ARTIFACT_SIGNING_DLIB")
    metadata_value = os.environ.get("DESKLINK_ARTIFACT_SIGNING_METADATA")
    thumbprint_value = os.environ.get("DESKLINK_SIGN_CERT_SHA1")

    artifact_mode_requested = bool(dlib_value or metadata_value)
    certificate_mode_requested = bool(thumbprint_value)
    if artifact_mode_requested and certificate_mode_requested:
        raise SystemExit(
            "Choose either Microsoft Artifact Signing or a certificate-store thumbprint, not both."
        )

    if artifact_mode_requested:
        if not dlib_value or not metadata_value:
            raise SystemExit(
                "Artifact Signing requires both DESKLINK_ARTIFACT_SIGNING_DLIB and "
                "DESKLINK_ARTIFACT_SIGNING_METADATA."
            )
        dlib = Path(dlib_value).expanduser()
        metadata = Path(metadata_value).expanduser()
        if not dlib.is_file():
            raise SystemExit(f"Artifact Signing dlib does not exist: {dlib}")
        if not metadata.is_file():
            raise SystemExit(f"Artifact Signing metadata does not exist: {metadata}")
        return SigningConfiguration(
            mode="artifact-signing",
            dlib=dlib.resolve(),
            metadata=metadata.resolve(),
            timestamp_url=ARTIFACT_SIGNING_TIMESTAMP_URL,
        )

    if certificate_mode_requested:
        thumbprint = re.sub(r"\s+", "", thumbprint_value or "").upper()
        if not re.fullmatch(r"[0-9A-F]{40}", thumbprint):
            raise SystemExit(
                "DESKLINK_SIGN_CERT_SHA1 must be a 40-character SHA-1 certificate thumbprint."
            )
        return SigningConfiguration(
            mode="certificate-store",
            thumbprint=thumbprint,
            timestamp_url=os.environ.get(
                "DESKLINK_TIMESTAMP_URL", DEFAULT_TIMESTAMP_URL
            ),
        )

    raise SystemExit(
        "No signing identity is configured. Set the Artifact Signing variables or "
        "DESKLINK_SIGN_CERT_SHA1."
    )


def signing_command(
    signtool: Path, configuration: SigningConfiguration, artifact: Path
) -> list[str]:
    command = [
        str(signtool),
        "sign",
        "/v",
        "/fd",
        "SHA256",
        "/tr",
        configuration.timestamp_url,
        "/td",
        "SHA256",
        "/d",
        "DeskLink",
    ]
    if configuration.mode == "artifact-signing":
        assert configuration.dlib is not None
        assert configuration.metadata is not None
        command.extend(
            [
                "/dlib",
                str(configuration.dlib),
                "/dmdf",
                str(configuration.metadata),
            ]
        )
    else:
        assert configuration.thumbprint is not None
        command.extend(["/s", "My", "/sha1", configuration.thumbprint])
    command.append(str(artifact))
    return command


def verification_command(signtool: Path, artifact: Path) -> list[str]:
    return [str(signtool), "verify", "/pa", "/all", "/v", "/tw", str(artifact)]


def run(command: list[str], *, dry_run: bool) -> None:
    print("+", subprocess.list2cmdline(command), flush=True)
    if not dry_run:
        result = subprocess.run(command)
        if result.returncode:
            raise SystemExit(f"SignTool failed with exit code {result.returncode}.")


def main() -> int:
    if os.name != "nt":
        raise SystemExit("Windows code signing must run on Windows.")
    sys.stdout.reconfigure(encoding="utf-8")

    arguments = parse_args()
    artifact = arguments.artifact.expanduser().resolve()
    if not artifact.is_file():
        raise SystemExit(f"Artifact does not exist: {artifact}")

    signtool = find_signtool()
    if not arguments.verify_only:
        configuration = read_signing_configuration()
        print(f"Signing {artifact.name} via {configuration.mode}.", flush=True)
        run(signing_command(signtool, configuration, artifact), dry_run=arguments.dry_run)

    print(f"Verifying {artifact.name} with the Authenticode policy.", flush=True)
    run(verification_command(signtool, artifact), dry_run=arguments.dry_run)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
