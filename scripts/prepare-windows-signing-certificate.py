#!/usr/bin/env python3
"""Import a CI-provided PFX into the Windows user certificate store safely."""

from __future__ import annotations

import base64
import binascii
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path


PFX_BASE64_ENV = "DESKLINK_SIGNING_PFX_BASE64"
PFX_PASSWORD_ENV = "DESKLINK_SIGNING_PFX_PASSWORD"
CODE_SIGNING_EKU = "1.3.6.1.5.5.7.3.3"


def decode_pfx(value: str) -> bytes:
    compact = "".join(value.split())
    if not compact:
        raise SystemExit(f"{PFX_BASE64_ENV} is empty.")
    try:
        payload = base64.b64decode(compact, validate=True)
    except (binascii.Error, ValueError) as error:
        raise SystemExit(f"{PFX_BASE64_ENV} is not valid base64.") from error
    if not payload:
        raise SystemExit(f"{PFX_BASE64_ENV} decoded to an empty file.")
    return payload


def write_github_environment(name: str, value: str) -> bool:
    github_environment = os.environ.get("GITHUB_ENV")
    if not github_environment:
        return False
    with Path(github_environment).open("a", encoding="utf-8", newline="\n") as output:
        output.write(f"{name}={value}\n")
    return True


def import_certificate(pfx_path: Path) -> str:
    script = rf"""
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$password = ConvertTo-SecureString $env:{PFX_PASSWORD_ENV} -AsPlainText -Force
$imported = @(Import-PfxCertificate `
    -FilePath $env:DESKLINK_SIGNING_PFX_PATH `
    -CertStoreLocation Cert:\CurrentUser\My `
    -Password $password)
$now = Get-Date
$matching = @($imported | Where-Object {{
    $_.HasPrivateKey -and
    $_.NotBefore -le $now -and
    $_.NotAfter -gt $now -and
    @($_.EnhancedKeyUsageList | ForEach-Object {{ $_.ObjectId.Value }}) -contains '{CODE_SIGNING_EKU}'
}})
if ($matching.Count -ne 1) {{
    foreach ($certificate in $imported) {{
        Remove-Item -LiteralPath "Cert:\CurrentUser\My\$($certificate.Thumbprint)" `
            -Force -ErrorAction SilentlyContinue
    }}
    throw 'The PFX must contain exactly one currently valid certificate with a private key and the Code Signing EKU.'
}}
$matching[0].Thumbprint
"""
    environment = os.environ.copy()
    environment["DESKLINK_SIGNING_PFX_PATH"] = str(pfx_path)
    result = subprocess.run(
        ["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script],
        env=environment,
        check=True,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    output = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    thumbprint = output[-1].upper() if output else ""
    if not re.fullmatch(r"[0-9A-F]{40}", thumbprint):
        raise SystemExit("The imported certificate did not return a valid SHA-1 thumbprint.")
    return thumbprint


def main() -> int:
    if os.name != "nt":
        raise SystemExit("A Windows certificate can only be imported on Windows.")
    sys.stdout.reconfigure(encoding="utf-8")

    encoded_pfx = os.environ.get(PFX_BASE64_ENV)
    password = os.environ.get(PFX_PASSWORD_ENV)
    if encoded_pfx is None:
        raise SystemExit(f"Missing required environment variable: {PFX_BASE64_ENV}")
    if password is None:
        raise SystemExit(f"Missing required environment variable: {PFX_PASSWORD_ENV}")

    handle, temporary_name = tempfile.mkstemp(prefix="desklink-signing-", suffix=".pfx")
    temporary_path = Path(temporary_name)
    try:
        with os.fdopen(handle, "wb") as output:
            output.write(decode_pfx(encoded_pfx))
        thumbprint = import_certificate(temporary_path)
    finally:
        temporary_path.unlink(missing_ok=True)

    exported = write_github_environment("DESKLINK_SIGN_CERT_SHA1", thumbprint)
    destination = "GITHUB_ENV" if exported else "the current shell output"
    print(f"Imported a valid code-signing certificate; thumbprint exported to {destination}.")
    if not exported:
        print(f"DESKLINK_SIGN_CERT_SHA1={thumbprint}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
