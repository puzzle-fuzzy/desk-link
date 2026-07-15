#!/usr/bin/env python3

from __future__ import annotations

import hashlib
import io
import json
import ssl
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "server" / "relay" / "Cargo.toml"
OUTPUT_DIRECTORY = ROOT / "dist" / "linux"
BUILDER_IMAGE = "oven/bun:1.3"
CARGO_CACHE_VOLUME = "desklink-relay-cargo-cache"
RUSTUP_CACHE_VOLUME = "desklink-relay-rustup-cache"
CONTAINER_TARGET_DIRECTORY = ROOT / "target" / "linux-amd64"
RUST_VERSION = "1.88.0"


def run(command: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
    print("+", subprocess.list2cmdline(command), flush=True)
    return subprocess.run(
        command,
        cwd=ROOT,
        check=True,
        encoding="utf-8",
        errors="replace",
        **kwargs,
    )


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def export_windows_trust_store(path: Path) -> None:
    certificates: set[bytes] = set()
    for store in ("ROOT", "CA"):
        for certificate, encoding, _trust in ssl.enum_certificates(store):
            if encoding == "x509_asn":
                certificates.add(certificate)
    if not certificates:
        raise RuntimeError("Windows trust store did not contain any X.509 certificates")
    pem = "".join(
        ssl.DER_cert_to_PEM_cert(certificate) for certificate in sorted(certificates)
    )
    path.write_text(pem, encoding="ascii", newline="\n")


def build_static_binary() -> Path:
    run(["docker", "image", "inspect", BUILDER_IMAGE], capture_output=True)
    source_mount = f"type=bind,source={ROOT},target=/src"
    cache_mount = f"type=volume,source={CARGO_CACHE_VOLUME},target=/root/.cargo"
    rustup_mount = f"type=volume,source={RUSTUP_CACHE_VOLUME},target=/root/.rustup"
    rustup_url = (
        "https://static.rust-lang.org/rustup/dist/"
        "x86_64-unknown-linux-gnu/rustup-init"
    )
    with tempfile.TemporaryDirectory(
        prefix="desklink-relay-trust-", dir=OUTPUT_DIRECTORY
    ) as trust_directory:
        trust_bundle = Path(trust_directory) / "windows-trust.pem"
        export_windows_trust_store(trust_bundle)
        trust_bundle_in_container = "/src/" + trust_bundle.relative_to(ROOT).as_posix()
        apt_tls = f"-o Acquire::https::CaInfo={trust_bundle_in_container}"
        build_command = " && ".join(
            [
                f"export SSL_CERT_FILE={trust_bundle_in_container}",
                f"export NODE_EXTRA_CA_CERTS={trust_bundle_in_container}",
                f"export CARGO_HTTP_CAINFO={trust_bundle_in_container}",
            (
                "sed -i "
                "-e 's|http://deb.debian.org/debian-security|"
                "http://mirrors.cloud.tencent.com/debian-security|g' "
                "-e 's|http://deb.debian.org/debian|"
                "http://mirrors.cloud.tencent.com/debian|g' "
                "/etc/apt/sources.list.d/debian.sources"
            ),
            f"apt-get {apt_tls} update -qq",
            (
                f"DEBIAN_FRONTEND=noninteractive apt-get {apt_tls} install -y -qq "
                "--no-install-recommends build-essential ca-certificates curl"
            ),
            "export PATH=/root/.cargo/bin:$PATH",
            "export RUSTUP_USE_CURL=1",
            (
                "if ! command -v rustup >/dev/null 2>&1; then "
                f"curl --fail --location --silent --show-error '{rustup_url}' "
                "--output /tmp/rustup-init; "
                "chmod 0755 /tmp/rustup-init; "
                f"/tmp/rustup-init -y --profile minimal --default-toolchain {RUST_VERSION}; "
                "rm -f /tmp/rustup-init; fi"
            ),
            f"rustup toolchain install {RUST_VERSION} --profile minimal",
            f'test "$(rustc +{RUST_VERSION} --version | cut -d" " -f2)" = "{RUST_VERSION}"',
            (
                "CARGO_TARGET_DIR=/src/target/linux-amd64 "
                f"cargo +{RUST_VERSION} rustc --locked --release "
                "--package desklink-relay --bin desklink-relay -- "
                "-C target-feature=+crt-static"
            ),
            (
                "! readelf -l /src/target/linux-amd64/release/desklink-relay "
                "| grep -q 'Requesting program interpreter'"
            ),
            ]
        )
        run(
            [
                "docker",
                "run",
                "--rm",
                "--user",
                "0:0",
                "--mount",
                source_mount,
                "--mount",
                cache_mount,
                "--mount",
                rustup_mount,
                "--workdir",
                "/src",
                BUILDER_IMAGE,
                "sh",
                "-lc",
                build_command,
            ]
        )
    binary = CONTAINER_TARGET_DIRECTORY / "release" / "desklink-relay"
    if not binary.is_file() or binary.stat().st_size == 0:
        raise RuntimeError("Linux relay binary was not produced")
    return binary


def import_minimal_image(binary: Path, image: str) -> str:
    with tempfile.TemporaryDirectory(prefix="desklink-relay-", dir=OUTPUT_DIRECTORY) as directory:
        rootfs = Path(directory) / "rootfs.tar"
        payload = binary.read_bytes()
        with tarfile.open(rootfs, "w") as archive:
            entry = tarfile.TarInfo("usr/local/bin/desklink-relay")
            entry.size = len(payload)
            entry.mode = 0o755
            entry.uid = 65534
            entry.gid = 65534
            entry.uname = "nobody"
            entry.gname = "nogroup"
            entry.mtime = 0
            archive.addfile(entry, io.BytesIO(payload))
        imported = run(
            [
                "docker",
                "import",
                "--platform",
                "linux/amd64",
                "--change",
                'LABEL org.opencontainers.image.title="DeskLink Relay"',
                "--change",
                "ENV DESKLINK_RELAY_ADDR=0.0.0.0:4433",
                "--change",
                "ENV DESKLINK_RELAY_SESSION_TTL_S=86400",
                "--change",
                "EXPOSE 4433/udp",
                "--change",
                "USER 65534:65534",
                "--change",
                'ENTRYPOINT ["/usr/local/bin/desklink-relay"]',
                str(rootfs),
                image,
            ],
            capture_output=True,
        )
    return imported.stdout.strip()


def smoke_test(image: str) -> None:
    check = run(
        ["docker", "run", "--rm", "--read-only", "--cap-drop", "ALL", image, "--check-config"],
        capture_output=True,
    )
    if "DeskLink relay configuration is valid" not in f"{check.stdout}\n{check.stderr}":
        raise RuntimeError("relay configuration check did not succeed")

    container_name = f"desklink-relay-build-check-{int(time.time())}"
    try:
        run(
            [
                "docker",
                "run",
                "--detach",
                "--name",
                container_name,
                "--read-only",
                "--cap-drop",
                "ALL",
                "--memory",
                "256m",
                image,
            ],
            capture_output=True,
        )
        time.sleep(2)
        inspection = run(
            ["docker", "inspect", "--format", "{{.State.Running}}", container_name],
            capture_output=True,
        )
        if inspection.stdout.strip() != "true":
            raise RuntimeError("relay container did not remain running")
        logs = run(["docker", "logs", container_name], capture_output=True)
        if "DeskLink relay listening on 0.0.0.0:4433" not in f"{logs.stdout}\n{logs.stderr}":
            raise RuntimeError("relay startup log was not observed")
    finally:
        subprocess.run(
            ["docker", "rm", "--force", container_name],
            cwd=ROOT,
            check=False,
            capture_output=True,
            encoding="utf-8",
            errors="replace",
        )


def main() -> int:
    manifest = tomllib.loads(MANIFEST.read_text(encoding="utf-8"))
    version = str(manifest["package"]["version"])
    image = f"desklink-relay:{version}"
    archive = OUTPUT_DIRECTORY / f"desklink-relay-{version}-amd64.tar"

    OUTPUT_DIRECTORY.mkdir(parents=True, exist_ok=True)
    binary = build_static_binary()
    image_id = import_minimal_image(binary, image)
    smoke_test(image)
    run(["docker", "save", "--output", str(archive), image])
    result = {
        "image": image,
        "image_id": image_id,
        "binary_size_bytes": binary.stat().st_size,
        "binary_sha256": sha256(binary),
        "archive": str(archive),
        "archive_size_bytes": archive.stat().st_size,
        "archive_sha256": sha256(archive),
        "platform": "linux/amd64",
    }
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, subprocess.CalledProcessError, RuntimeError, KeyError) as error:
        print(f"build failed: {error}", file=sys.stderr)
        raise SystemExit(1)
