#!/usr/bin/env python3
"""Verify that the Windows release executable is built from the current UI sources."""

from __future__ import annotations

import argparse
import hashlib
from html.parser import HTMLParser
import json
import os
import posixpath
import re
import struct
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit

SCRIPTS_DIRECTORY = Path(__file__).resolve().parent
if str(SCRIPTS_DIRECTORY) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIRECTORY))

from windows_native_build_env import (
    prepare_windows_native_build_environment,
    prepare_windows_release_environment,
)
from cargo_manifest import package_version


ROOT = Path(__file__).resolve().parents[1]
WINDOWS_UI = ROOT / "apps" / "windows-ui"
TARGET = "x86_64-pc-windows-msvc"
README = ROOT / "README.md"
COMMIT_SHA = re.compile(r"^[0-9a-fA-F]{40}$")
BUN_VERSION = re.compile(r"^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$")
PRODUCT_CONFIG = WINDOWS_UI / "src" / "product-config.ts"
RUST_RELAY_CONFIG = WINDOWS_UI / "src-tauri" / "src" / "local_relay.rs"
WINDOWS_ASSETS = ROOT / "apps" / "windows" / "assets"
PRODUCTION_RUST_SOURCE_ROOTS = (
    ROOT / "apps" / "windows" / "src",
    ROOT / "apps" / "windows-ui" / "src-tauri" / "src",
)


def run(command: list[str], *, cwd: Path = ROOT) -> None:
    print("+", subprocess.list2cmdline(command), flush=True)
    subprocess.run(command, cwd=cwd, check=True)


def cargo_version(path: str) -> str:
    return package_version(ROOT / path)


def source_metadata() -> tuple[str, bool]:
    """Return the source commit and whether the checkout has local changes."""
    commit = os.environ.get("GITHUB_SHA", "").strip()
    if commit and not COMMIT_SHA.fullmatch(commit):
        raise SystemExit("GITHUB_SHA must contain a 40-character commit SHA")
    if not commit:
        result = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            encoding="utf-8",
            check=False,
        )
        commit = result.stdout.strip()
        if result.returncode != 0 or not COMMIT_SHA.fullmatch(commit):
            raise SystemExit("Could not determine the current source commit SHA")
    status = subprocess.run(
        ["git", "status", "--porcelain", "--untracked-files=all"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    if status.returncode != 0:
        raise SystemExit("Could not inspect the source checkout status")
    return commit.lower(), bool(status.stdout.strip())


def verify_versions() -> str:
    versions = {
        "windows-runtime": cargo_version("apps/windows/Cargo.toml"),
        "windows-ui-rust": cargo_version("apps/windows-ui/src-tauri/Cargo.toml"),
        "windows-installer": cargo_version("tools/windows-installer/Cargo.toml"),
        "windows-ui-package": str(
            json.loads((WINDOWS_UI / "package.json").read_text(encoding="utf-8"))["version"]
        ),
        "tauri-config": str(
            json.loads(
                (WINDOWS_UI / "src-tauri" / "tauri.conf.json").read_text(
                    encoding="utf-8"
                )
            )["version"]
        ),
    }
    unique = set(versions.values())
    if len(unique) != 1:
        detail = ", ".join(f"{name}={version}" for name, version in versions.items())
        raise SystemExit(f"Windows release versions do not match: {detail}")
    return unique.pop()


def current_source_versions() -> dict[str, object]:
    """Read source metadata without building, connecting or touching a device."""
    protocol = required_match(
        ROOT / "crates/desklink-protocol/src/lib.rs",
        r"pub const PROTOCOL_VERSION: u16 = (\d+);",
        "application protocol version",
    )
    toolchain = required_match(
        ROOT / "rust-toolchain.toml",
        r'(?m)^channel\s*=\s*"([^"]+)"',
        "Rust toolchain",
    )
    return {
        "windows_version": verify_versions(),
        "protocol_version": int(protocol),
        "bun_version": expected_bun_version(),
        "rust_toolchain": toolchain,
    }


def expected_bun_version() -> str:
    package = json.loads((WINDOWS_UI / "package.json").read_text(encoding="utf-8"))
    package_manager = package.get("packageManager")
    if not isinstance(package_manager, str) or not package_manager.startswith("bun@"):
        raise SystemExit("Windows UI packageManager must pin Bun")
    version = package_manager.removeprefix("bun@").strip()
    if not BUN_VERSION.fullmatch(version):
        raise SystemExit("Windows UI packageManager contains an invalid Bun version")
    return version


def verify_bun_version() -> str:
    expected = expected_bun_version()
    completed = subprocess.run(
        ["bun", "--version"],
        cwd=WINDOWS_UI,
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    actual = completed.stdout.strip()
    if completed.returncode != 0 or actual != expected:
        raise SystemExit(
            f"Bun {expected} is required for the Windows release; detected {actual or 'unavailable'}"
        )
    return actual


def required_match(path: Path, pattern: str, label: str) -> str:
    match = re.search(pattern, path.read_text(encoding="utf-8"))
    if not match:
        raise SystemExit(f"Could not read {label} from {path.relative_to(ROOT)}")
    return match.group(1)


def verify_managed_relay_profile() -> dict[str, str]:
    names = ("MANAGED_RELAY_ADDRESS", "MANAGED_RELAY_SERVER_NAME")
    frontend = {
        name: required_match(
            PRODUCT_CONFIG,
            rf'export\s+const\s+{name}\s*=\s*"([^"]+)"',
            name,
        )
        for name in names
    }
    backend = {
        name: required_match(
            RUST_RELAY_CONFIG,
            rf'pub\s+const\s+{name}:\s*&str\s*=\s*"([^"]+)"',
            name,
        )
        for name in names
    }
    if frontend != backend:
        raise SystemExit(
            "Managed relay profile differs between TypeScript and Rust: "
            f"frontend={frontend}, backend={backend}"
        )
    return {
        "relay_address": frontend["MANAGED_RELAY_ADDRESS"],
        "tls_server_name": frontend["MANAGED_RELAY_SERVER_NAME"],
    }


def verify_release_scope(readme: Path = README) -> dict[str, object]:
    """Keep the public release description aligned with the Windows target."""
    text = readme.read_text(encoding="utf-8")
    required = (
        "Windows 10/11 x64",
        "当前正式发布目标是 Windows 10/11 x64",
        "跨平台研究代码",
    )
    for phrase in required:
        if phrase not in text:
            raise SystemExit(
                f"README release scope is missing the required phrase: {phrase}"
            )
    forbidden = (
        "当前仓库同时包含 Windows 10/11 x64 桌面端与 macOS Apple Silicon 桌面端",
        "## macOS 构建与使用",
        "两台 Apple Silicon Mac 完成",
    )
    for phrase in forbidden:
        if phrase in text:
            raise SystemExit(
                f"README still advertises a non-Windows release path: {phrase}"
            )
    return {
        "target": "windows-10/11-x64",
        "macos_release": False,
        "mobile_release": False,
    }


def verify_static_windows_assets() -> dict[str, dict[str, object]]:
    expected = {
        "desklink-icon.png": b"\x89PNG\r\n\x1a\n",
        "desklink.ico": b"\x00\x00\x01\x00",
    }
    verified: dict[str, dict[str, object]] = {}
    for name, signature in expected.items():
        path = WINDOWS_ASSETS / name
        if not path.is_file():
            raise SystemExit(f"Required Windows asset is missing: {path.relative_to(ROOT)}")
        data = path.read_bytes()
        if len(data) <= len(signature) or not data.startswith(signature):
            raise SystemExit(f"Windows asset has an invalid file signature: {path.relative_to(ROOT)}")
        verified[name] = {
            "size_bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest(),
        }
    return verified


def rust_code_mask(text: str) -> str:
    """Preserve offsets/delimiters while hiding Rust comments and literals."""
    masked = list(text)
    raw_pattern = re.compile(r'(?:br|cr|r)(#*)"')
    char_pattern = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^\\'\n])'")
    i = 0
    while i < len(text):
        start = i
        if text.startswith("//", i):
            end = text.find("\n", i)
            i = len(text) if end < 0 else end
        elif text.startswith("/*", i):
            depth = 1
            i += 2
            while i < len(text) and depth:
                if text.startswith("/*", i):
                    depth += 1
                    i += 2
                elif text.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
        elif text[i] in "rbc" and (raw := raw_pattern.match(text, i)) and (
            i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")
        ):
            terminator = '"' + raw.group(1)
            end = text.find(terminator, raw.end())
            i = len(text) if end < 0 else end + len(terminator)
        elif text[i] == '"':
            i += 1
            while i < len(text):
                if text[i] == "\\":
                    i += 2
                elif text[i] == '"':
                    i += 1
                    break
                else:
                    i += 1
        elif text[i] == "'" and (char := char_pattern.match(text, i)):
            i = char.end()
        else:
            i += 1
            continue
        masked[start:i] = ["\n" if c == "\n" else " " for c in text[start:i]]
    return "".join(masked)


def rust_delimiters(code: str) -> dict[int, int]:
    stack: list[tuple[str, int]] = []
    pairs: dict[int, int] = {}
    for index, character in enumerate(code):
        if character in "([{":
            stack.append((character, index))
        elif character in ")]}":
            if not stack or {"(": ")", "[": "]", "{": "}"}[stack[-1][0]] != character:
                # Invalid/unsupported source must not authorize an exclusion.
                return {}
            _, start = stack.pop()
            pairs[start] = index
    return pairs if not stack else {}


def rust_test_regions(text: str) -> tuple[str, list[tuple[int, int]], dict[int, str | None]]:
    """Exclude only the complete next item of an exact #[cfg(test)].

    This is a deliberately small lexical scanner, not a Rust cfg evaluator.
    Unknown cfg forms and unrecognized item syntax remain subject to the gate.
    """
    code = rust_code_mask(text)
    pairs = rust_delimiters(code)
    regions: list[tuple[int, int]] = []
    paths: dict[int, str | None] = {}
    processed = 0
    for attribute in re.finditer(r"#\s*\[", code):
        if attribute.start() < processed:
            continue
        cursor = attribute.start()
        test_only = False
        explicit_path = None
        has_path = False
        while match := re.match(r"#\s*\[", code[cursor:]):
            opening = cursor + match.end() - 1
            if opening not in pairs:
                break
            end = pairs[opening]
            test_only |= re.fullmatch(r"\s*cfg\s*\(\s*test\s*\)\s*", code[opening + 1:end]) is not None
            path = re.fullmatch(r'\s*path\s*=\s*(.*?)\s*', text[opening + 1:end], re.DOTALL)
            if path:
                has_path = True
                try:
                    explicit_path = json.loads(path.group(1))
                except ValueError:
                    raw_path = re.fullmatch(r'r(#*)"(.*)"\1', path.group(1), re.DOTALL)
                    explicit_path = raw_path.group(2) if raw_path else None
                if not isinstance(explicit_path, str):
                    explicit_path = None
            cursor = end + 1
            while cursor < len(code) and code[cursor].isspace():
                cursor += 1
        processed = cursor
        visibility = r"(?:pub(?:\([^)]*\))?\s+)?"
        item = re.match(visibility + r"(?:(?:async|unsafe|const|extern)\s+)*(fn)\b|" + visibility + r"(mod|struct|enum|impl|trait|use|type|static|const|extern|macro_rules)\b", code[cursor:])
        if not item:
            continue
        if has_path:
            paths[cursor] = explicit_path
        if not test_only:
            continue
        position = cursor + item.end()
        while position < len(code):
            token = code[position]
            if token == ";":
                regions.append((attribute.start(), position + 1))
                break
            if token in "([{":
                if position not in pairs:
                    break
                end = pairs[position] + 1
                if token == "{" and (item.group(1) or item.group(2)) not in {"use", "type", "static", "const"}:
                    regions.append((attribute.start(), end))
                    break
                position = end
            else:
                position += 1
    return code, regions, paths


def production_rust_sources(files: dict[Path, str]) -> dict[Path, str]:
    """Resolve test-only module reachability; shared production bindings win."""
    scans = {path: rust_test_regions(text) for path, text in files.items()}
    edges: dict[Path, list[tuple[Path, bool]]] = {path: [] for path in files}
    incoming: set[Path] = set()
    for path, (code, regions, paths) in scans.items():
        pairs = rust_delimiters(code)
        modules = list(re.finditer(r"\bmod\s+([A-Za-z_][A-Za-z_0-9]*)\s*([;{])", code))
        def module_path(module: re.Match[str]) -> tuple[bool, str | None]:
            for start, value in paths.items():
                if start <= module.start() and re.fullmatch(r"(?:pub(?:\([^)]*\))?\s+)?", code[start:module.start()]):
                    return True, value
            return False, None
        for module in modules:
            if module.group(2) != ";":
                continue
            base = path.parent if path.name in {"lib.rs", "main.rs", "mod.rs"} else path.parent / path.stem
            ancestors = [parent for parent in modules if parent.group(2) == "{" and parent.end() - 1 in pairs
                         and parent.end() <= module.start() < pairs[parent.end() - 1]]
            if any(module_path(parent)[0] for parent in ancestors):
                # Inline #[path] resolution has additional Rust-specific base
                # rules. Leave those files scanned instead of guessing a target.
                continue
            for parent in ancestors:
                base /= parent.group(1)
            has_path, explicit = module_path(module)
            if has_path and explicit is None:
                continue
            if has_path:
                candidates = [(base if ancestors else path.parent) / explicit]
            else:
                candidates = [base / (module.group(1) + ".rs"), base / module.group(1) / "mod.rs"]
            targets = [candidate.resolve() for candidate in candidates if candidate.resolve() in files]
            if len(targets) != 1:
                continue
            target = targets[0]
            conditional = any(start <= module.start() < end for start, end in regions)
            edges[path].append((target, conditional))
            incoming.add(target)
    roots = set(files) - incoming
    reachable: set[tuple[Path, bool]] = set()
    pending = [(path, False) for path in roots]
    while pending:
        path, test_only = pending.pop()
        if (path, test_only) in reachable:
            continue
        reachable.add((path, test_only))
        pending.extend((target, test_only or conditional) for target, conditional in edges[path])
    test_only_files = {path for path, conditional in reachable if conditional} - {
        path for path, conditional in reachable if not conditional
    }
    production = {}
    for path, (code, regions, _) in scans.items():
        if path in test_only_files:
            continue
        masked = list(code)
        for start, end in regions:
            masked[start:end] = " " * (end - start)
        production[path] = "".join(masked)
    return production


def verify_production_backpressure(
    source_roots: tuple[Path, ...] = PRODUCTION_RUST_SOURCE_ROOTS,
) -> dict[str, object]:
    """Reject unbounded Rust channels in the Windows production data plane."""

    forbidden = (
        ("tokio::sync::mpsc::unbounded_channel", re.compile(r"\bunbounded_channel\s*\(")),
        ("std::sync::mpsc::channel", re.compile(r"std::sync::mpsc::channel\s*\(")),
        # `mpsc::channel()` is also the unbounded std channel when the import
        # is aliased. Bounded Tokio channels always carry an explicit capacity.
        ("unbounded mpsc::channel", re.compile(r"\bmpsc::channel\s*\(\s*\)")),
    )
    files: dict[Path, str] = {}
    violations: list[str] = []
    for source_root in source_roots:
        if not source_root.is_dir():
            raise SystemExit(f"Windows production Rust source directory is missing: {source_root}")
        for path in sorted(source_root.rglob("*.rs")):
            files[path.resolve()] = path.read_text(encoding="utf-8")
    for path, code in production_rust_sources(files).items():
        for label, pattern in forbidden:
            if pattern.search(code):
                try:
                    display_path = path.relative_to(ROOT)
                except ValueError:
                    display_path = path
                violations.append(f"{display_path} ({label})")
    if violations:
        detail = ", ".join(violations)
        raise SystemExit(
            "Windows production Rust contains an unbounded channel; use a bounded "
            f"queue with explicit backpressure: {detail}"
        )
    return {"checked_files": len(files), "unbounded_channels": 0}


def verify_frontend_assets() -> list[str]:
    distribution = WINDOWS_UI / "dist"
    index = distribution / "index.html"
    if not index.is_file():
        raise SystemExit("Windows UI dist/index.html was not produced")
    assets = sorted(
        path.relative_to(distribution).as_posix()
        for path in distribution.rglob("*")
        if path.is_file() and path.suffix in {".css", ".js"}
    )
    if not assets or not any(path.endswith(".js") for path in assets):
        raise SystemExit("Windows UI production assets were not produced")
    forbidden_urls = (
        "http://localhost",
        "https://localhost",
        "http://127.0.0.1",
        "https://127.0.0.1",
    )
    sources = {}
    for relative_path in ["index.html", *assets]:
        source = (distribution / relative_path).read_text(encoding="utf-8")
        sources[relative_path] = source
        found = next((url for url in forbidden_urls if url in source), None)
        if found:
            raise SystemExit(
                f"Production asset {relative_path} contains development URL {found}"
            )

    def local_asset(reference: str, owner: str) -> str | None:
        url = urlsplit(reference.replace("\\/", "/"))
        if url.scheme or url.netloc:
            return None
        path = unquote(url.path)
        if (
            not path.endswith((".js", ".css"))
            or posixpath.basename(path) in {".js", ".css"}
            or "${" in path
        ):
            return None
        relative = posixpath.normpath(
            path.lstrip("/") if path.startswith("/")
            else posixpath.join(posixpath.dirname(owner), path)
        )
        if relative.startswith("../") or relative not in sources:
            raise SystemExit(f"Production asset {owner} references missing local asset {reference}")
        return relative

    class IndexReferences(HTMLParser):
        def __init__(self) -> None:
            super().__init__()
            self.assets: set[str] = set()
            self.scripts: set[str] = set()

        def handle_starttag(self, tag: str, attributes: list[tuple[str, str | None]]) -> None:
            attribute = "src" if tag == "script" else "href" if tag == "link" else None
            if attribute is None:
                return
            reference = dict(attributes).get(attribute)
            dependency = local_asset(reference, "index.html") if reference else None
            if dependency is not None:
                self.assets.add(dependency)
                if tag == "script" and dependency.endswith(".js"):
                    self.scripts.add(dependency)

    entries = IndexReferences()
    entries.feed(sources["index.html"])
    if not entries.scripts:
        raise SystemExit("Production index.html has no local JavaScript entry")

    # Vite emits imports, CSS URLs and ?url worklet loaders as static strings.
    # Follow those strings transitively rather than requiring every chunk in HTML.
    quoted_url = re.compile(r'''(["'`])([^"'`\r\n]*)\1''')
    css_url = re.compile(r"url\(\s*([^\s'\"()]+)\s*\)", re.IGNORECASE)
    pending = list(entries.assets)
    reachable: set[str] = set()
    while pending:
        owner = pending.pop()
        if owner in reachable:
            continue
        reachable.add(owner)
        references = [match.group(2) for match in quoted_url.finditer(sources[owner])]
        if owner.endswith(".css"):
            references.extend(match.group(1) for match in css_url.finditer(sources[owner]))
        for reference in references:
            dependency = local_asset(reference, owner)
            if dependency is not None and dependency not in reachable:
                pending.append(dependency)
    unreachable = sorted(set(assets) - reachable)
    if unreachable:
        raise SystemExit(f"Production assets are not reachable from index.html: {', '.join(unreachable)}")
    return assets


def verify_pe(path: Path) -> dict[str, object]:
    data = path.read_bytes()
    if len(data) < 0x100 or data[:2] != b"MZ":
        raise SystemExit(f"Windows release is not a PE executable: {path}")
    pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise SystemExit(f"Windows release has an invalid PE header: {path}")
    machine = struct.unpack_from("<H", data, pe_offset + 4)[0]
    if machine != 0x8664:
        raise SystemExit(f"Windows release is not x64 (machine=0x{machine:04x})")
    digest = hashlib.sha256(data).hexdigest()
    return {"path": str(path), "size_bytes": len(data), "sha256": digest}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--versions-only", action="store_true",
        help="print versions read from source manifests; no build or device access",
    )
    args = parser.parse_args()
    if args.versions_only:
        print(json.dumps(current_source_versions(), indent=2))
        return 0
    if os.name != "nt":
        raise SystemExit("Windows release verification must run on Windows")
    prepare_windows_native_build_environment()
    prepare_windows_release_environment()
    verify_bun_version()
    version = verify_versions()
    source_commit, source_dirty = source_metadata()
    release_scope = verify_release_scope()
    managed_relay = verify_managed_relay_profile()
    windows_assets = verify_static_windows_assets()
    backpressure = verify_production_backpressure()
    run(["bun", "install", "--frozen-lockfile"], cwd=WINDOWS_UI)
    run(["bun", "run", "test"], cwd=WINDOWS_UI)
    run(["bun", "run", "build"], cwd=WINDOWS_UI)
    assets = verify_frontend_assets()
    run(
        [
            "cargo",
            "build",
            "--release",
            "--target",
            TARGET,
            "--package",
            "desklink-windows-ui",
            "--bin",
            "desklink-windows-ui",
            "--features",
            "custom-protocol",
        ]
    )
    executable = ROOT / "target" / TARGET / "release" / "desklink-windows-ui.exe"
    release = verify_pe(executable)
    report = {
        "schema": 1,
        "version": version,
        "source_commit": source_commit,
        "source_dirty": source_dirty,
        "custom_protocol": True,
        "release_scope": release_scope,
        "frontend_assets": assets,
        "managed_relay": managed_relay,
        "windows_assets": windows_assets,
        "production_backpressure": backpressure,
        "release": release,
        "passed": True,
    }
    report_path = ROOT / "dist" / "windows" / "windows-release-verification.json"
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"Windows release verification passed: {report_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
