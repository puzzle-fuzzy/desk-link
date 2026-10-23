"""Independent Ed25519 authorization for the Windows update manifest.

The release key is DPAPI protected outside the checkout. Only its public half
and detached signatures may be distributed. Authenticode remains a separate
formal Windows release gate.
"""

from __future__ import annotations

import argparse
import ctypes
from ctypes import wintypes
import os
from pathlib import Path
import re

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey

ROOT = Path(__file__).resolve().parents[1]
PUBLIC_KEY = ROOT / "apps/windows-ui/src-tauri/update-public-key.hex"
DOMAIN = b"desklink-windows-update-manifest-v1\0"
MAX_MANIFEST_BYTES = 100_000


def default_key_path() -> Path:
    local = os.environ.get("LOCALAPPDATA")
    if not local:
        raise ValueError("LOCALAPPDATA is unavailable")
    return Path(local) / "DeskLinkRelease" / "update-signing-key.dpapi"


def configured_key_path() -> Path:
    value = os.environ.get("DESKLINK_UPDATE_SIGNING_KEY")
    return Path(value).expanduser() if value else default_key_path()


def _external_key_path(path: Path) -> Path:
    resolved = path.resolve()
    if resolved.is_relative_to(ROOT):
        raise ValueError("Update signing keys must be stored outside the checkout")
    return resolved


class _Blob(ctypes.Structure):
    _fields_ = [("cbData", wintypes.DWORD), ("pbData", ctypes.POINTER(ctypes.c_ubyte))]


def _dpapi(data: bytes, *, protect: bool) -> bytes:
    if os.name != "nt":
        raise ValueError("The release key requires Windows current-user DPAPI")
    buffer = (ctypes.c_ubyte * len(data)).from_buffer_copy(data)
    source = _Blob(len(data), buffer)
    output = _Blob()
    crypt32 = ctypes.WinDLL("crypt32", use_last_error=True)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.LocalFree.argtypes = [ctypes.c_void_p]
    kernel32.LocalFree.restype = ctypes.c_void_p
    # UI_FORBIDDEN; intentionally no LOCAL_MACHINE flag.
    if protect:
        operation = crypt32.CryptProtectData
        operation.argtypes = [ctypes.POINTER(_Blob), wintypes.LPCWSTR,
            ctypes.POINTER(_Blob), ctypes.c_void_p, ctypes.c_void_p,
            wintypes.DWORD, ctypes.POINTER(_Blob)]
        result = operation(ctypes.byref(source), "DeskLink update release key", None,
            None, None, 1, ctypes.byref(output))
    else:
        operation = crypt32.CryptUnprotectData
        operation.argtypes = [ctypes.POINTER(_Blob), ctypes.c_void_p,
            ctypes.POINTER(_Blob), ctypes.c_void_p, ctypes.c_void_p,
            wintypes.DWORD, ctypes.POINTER(_Blob)]
        result = operation(ctypes.byref(source), None, None, None, None, 1, ctypes.byref(output))
    operation.restype = wintypes.BOOL
    if not result:
        raise ValueError("Windows could not protect or unlock the update signing key")
    try:
        return ctypes.string_at(output.pbData, output.cbData)
    finally:
        ctypes.memset(output.pbData, 0, output.cbData)
        kernel32.LocalFree(output.pbData)
        ctypes.memset(buffer, 0, len(buffer))


def public_bytes(key: Ed25519PrivateKey) -> bytes:
    return key.public_key().public_bytes(serialization.Encoding.Raw, serialization.PublicFormat.Raw)


def read_public_key(path: Path = PUBLIC_KEY) -> bytes:
    value = _without_line_ending(path.read_bytes()).decode("ascii")
    if not re.fullmatch(r"[0-9a-f]{64}", value):
        raise ValueError("The embedded update public key is invalid")
    return bytes.fromhex(value)


def _without_line_ending(value: bytes) -> bytes:
    if value.endswith(b"\r\n"):
        return value[:-2]
    return value.removesuffix(b"\n")


def create_release_key(path: Path, public_path: Path = PUBLIC_KEY) -> None:
    path = _external_key_path(path)
    if path.exists() or public_path.exists():
        raise ValueError("An update key already exists; key rotation must be planned explicitly")
    key = Ed25519PrivateKey.generate()
    secret = key.private_bytes(serialization.Encoding.Raw,
        serialization.PrivateFormat.Raw, serialization.NoEncryption())
    protected = _dpapi(secret, protect=True)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("xb") as output:
        output.write(protected)
    try:
        with public_path.open("x", encoding="ascii", newline="\n") as output:
            output.write(public_bytes(key).hex() + "\n")
    except BaseException:
        path.unlink()
        raise


def load_release_key(path: Path, public_path: Path = PUBLIC_KEY) -> Ed25519PrivateKey:
    path = _external_key_path(path)
    protected = path.read_bytes()
    if not 1 <= len(protected) <= 16_384:
        raise ValueError("The protected update key is invalid")
    secret = _dpapi(protected, protect=False)
    if len(secret) != 32:
        raise ValueError("The unlocked update key is invalid")
    key = Ed25519PrivateKey.from_private_bytes(secret)
    if public_bytes(key) != read_public_key(public_path):
        raise ValueError("Update signing key does not match the embedded public key")
    return key


def sign_manifest_bytes(raw: bytes, key: Ed25519PrivateKey) -> bytes:
    if not 1 <= len(raw) <= MAX_MANIFEST_BYTES:
        raise ValueError("Update manifest exceeds its size limit")
    return (key.sign(DOMAIN + raw).hex() + "\n").encode("ascii")


def verify_manifest_signature(raw: bytes, signature: bytes, public_key: bytes) -> None:
    if not 1 <= len(raw) <= MAX_MANIFEST_BYTES:
        raise ValueError("Update manifest exceeds its size limit")
    if len(signature) > 130:
        raise ValueError("Update signature exceeds its size limit")
    try:
        value = _without_line_ending(signature).decode("ascii")
        if not re.fullmatch(r"[0-9a-f]{128}", value):
            raise ValueError("Update signature is invalid")
        Ed25519PublicKey.from_public_bytes(public_key).verify(bytes.fromhex(value), DOMAIN + raw)
    except (UnicodeDecodeError, InvalidSignature) as error:
        raise ValueError("Update manifest signature verification failed") from error


def sign_manifest(path: Path, key: Ed25519PrivateKey) -> Path:
    raw = path.read_bytes()
    signature = sign_manifest_bytes(raw, key)
    verify_manifest_signature(raw, signature, read_public_key())
    destination = path.with_name(path.name + ".sig")
    destination.write_bytes(signature)
    return destination


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("init", help="create a current-user protected release key, without overwriting")
    sign = commands.add_parser("sign", help="authorize an exact update manifest")
    sign.add_argument("manifest", type=Path)
    verify = commands.add_parser("verify", help="verify a detached manifest signature")
    verify.add_argument("manifest", type=Path)
    arguments = parser.parse_args()
    try:
        if arguments.command == "init":
            create_release_key(configured_key_path())
            print(f"Created protected release key: {configured_key_path()}")
            print(f"Public verification key: {PUBLIC_KEY}")
        elif arguments.command == "sign":
            print(sign_manifest(arguments.manifest, load_release_key(configured_key_path())))
        else:
            path = arguments.manifest
            verify_manifest_signature(path.read_bytes(), path.with_name(path.name + ".sig").read_bytes(), read_public_key())
            print("Update manifest signature verified")
    except (OSError, ValueError) as error:
        raise SystemExit(str(error)) from error


if __name__ == "__main__":
    main()
