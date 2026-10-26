#!/usr/bin/env python3
"""Run a pinned production Rust HostMedia encoder; never translate its format."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[3]
PRODUCER = "a599246535b73f57e0425cbbfd18c7156e050bba"
LISTENER_PRODUCER = "6dbe53d64c60bbdd50af4b592ebef7471b800fcc"
CRATE = "desklink-host-media-contract-fixture"
SOURCE_PATHS = [
    "apps/windows/src/diagnostics.rs",
    "apps/windows/src/runtime/media_diagnostics.rs",
    "apps/windows/src/runtime/cursor_send.rs",
    "Cargo.toml", "Cargo.lock", "rust-toolchain.toml",
]


def command(*arguments: str, env: dict[str, str] | None = None) -> bytes:
    return subprocess.run(arguments, cwd=ROOT, env=env, check=True, stdout=subprocess.PIPE).stdout


def cursor_reason(source: str) -> str:
    """Keep the original derive, enum and impl; exclude unrelated transport code."""
    enum_at = source.index("pub enum CursorDisableReason {")
    start = source.rfind("#[derive(", 0, enum_at)
    implementation = source.index("impl CursorDisableReason {", enum_at)
    opening = source.index("{", implementation)
    depth = 1
    ending = opening + 1
    while depth:
        if source[ending] == "{":
            depth += 1
        elif source[ending] == "}":
            depth -= 1
        ending += 1
    snippet = source[start:ending] + "\n"
    if start < 0 or "use desklink_transport" in snippet:
        raise RuntimeError("unexpected cursor reason source boundary")
    return snippet


def selected_packages(lock: dict, manifest: dict) -> tuple[dict[str, dict], list[dict]]:
    packages = lock["package"]

    def select(name: str, version: str | None = None) -> dict:
        matches = [entry for entry in packages if entry["name"] == name
                   and (version is None or entry["version"] == version)]
        if len(matches) != 1:
            raise RuntimeError(f"ambiguous pinned dependency {name} {version or ''}")
        return matches[0]

    roots: dict[str, dict] = {}
    for name in ("serde", "thiserror"):
        requirement = manifest["workspace"]["dependencies"][name]
        if isinstance(requirement, dict):
            requirement = requirement["version"]
        if not re.fullmatch(r"\d+(?:\.\d+)*", requirement):
            raise RuntimeError(f"unsupported production version requirement: {name}")
        matches = [entry for entry in packages if entry["name"] == name
                   and (entry["version"] == requirement or entry["version"].startswith(requirement + "."))]
        if len(matches) != 1:
            raise RuntimeError(f"ambiguous locked root dependency: {name}")
        roots[name] = matches[0]
    closure: dict[tuple[str, str], dict] = {}

    def visit(entry: dict) -> None:
        key = (entry["name"], entry["version"])
        if key in closure:
            return
        closure[key] = entry
        for dependency in entry.get("dependencies", []):
            parts = dependency.split()
            visit(select(parts[0], parts[1] if len(parts) > 1 else None))

    for entry in roots.values():
        visit(entry)
    return roots, sorted(closure.values(), key=lambda entry: (entry["name"], entry["version"]))


def crate_files(scratch: Path, sources: dict[str, bytes], event: str = "cursor") -> None:
    manifest = tomllib.loads(sources["Cargo.toml"].decode())
    lock = tomllib.loads(sources["Cargo.lock"].decode())
    roots, packages = selected_packages(lock, manifest)
    package = manifest["workspace"]["package"]
    cargo = f'''[package]
name = "{CRATE}"
version = "0.0.0"
edition = "{package["edition"]}"
rust-version = "{package["rust-version"]}"

[workspace]

[dependencies]
serde = {{ version = "={roots["serde"]["version"]}", features = ["derive"] }}
thiserror = "={roots["thiserror"]["version"]}"
'''
    (scratch / "Cargo.toml").write_text(cargo, encoding="utf8", newline="\n")
    lines = ["# Generated dependency closure from the pinned producer Cargo.lock.",
             f'version = {lock["version"]}', "", "[[package]]", f'name = "{CRATE}"',
             'version = "0.0.0"', 'dependencies = ["serde", "thiserror"]', ""]
    for entry in packages:
        lines.append("[[package]]")
        for key in ("name", "version", "source", "checksum"):
            if key in entry:
                lines.append(f"{key} = {json.dumps(entry[key])}")
        if entry.get("dependencies"):
            # Only one version of each name is in this closure; Cargo's canonical
            # lock form drops disambiguating versions used by the larger workspace.
            dependencies = [value.split()[0] for value in entry["dependencies"]]
            if len({item["name"] for item in packages}) != len(packages):
                raise RuntimeError("fixture dependency closure needs version disambiguation")
            lines.append("dependencies = " + json.dumps(dependencies))
        lines.append("")
    (scratch / "Cargo.lock").write_text("\n".join(lines), encoding="utf8", newline="\n")
    for path, value in sources.items():
        destination = scratch / "source" / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(value)
    reason = cursor_reason(sources["apps/windows/src/runtime/cursor_send.rs"].decode("utf8"))
    (scratch / "cursor_disable_reason.rs").write_text(reason, encoding="utf8", newline="\n")
    main = r'''#![allow(dead_code)]
extern crate self as desklink_protocol;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum H264Profile { Main, High }
mod storage {
    pub fn downloads_path() -> Option<std::path::PathBuf> { panic!("unused storage stub") }
    pub fn local_app_data_path() -> Option<std::path::PathBuf> { panic!("unused storage stub") }
}
mod runtime {
    pub mod cursor_send {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/cursor_disable_reason.rs"));
    }
    pub use cursor_send::CursorDisableReason;
    pub mod media_diagnostics {
        // The unrelated callback fallback references serde_json. It must never
        // execute in this encoder-only harness; no serialization is substituted.
        mod serde_json {
            pub fn to_string<T: ?Sized>(_: &T) -> Result<String, &'static str> {
                panic!("unused observer fallback stub")
            }
        }
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/source/apps/windows/src/runtime/media_diagnostics.rs"));
    }
    pub use media_diagnostics::HostMediaDiagnostic;
    // DIRECT_LISTENER_REASON_REEXPORT
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum HostLifecycleEvent {
        Connecting { attempt: u32, stream_id: u64 },
        Available { stream_id: u64 },
        Connected { stream_id: u64 },
        Reconnecting { retry: u32, maximum_retries: u32, delay: std::time::Duration, reason: String },
        Stopped { reason: String },
    }
}
mod diagnostics {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/source/apps/windows/src/diagnostics.rs"));
    pub fn producer_line(event: crate::runtime::HostMediaDiagnostic) -> String {
        encode_event(&DiagnosticEvent::HostMedia(event))
    }
}
'''
    if event == "direct-listener":
        main = main.replace("pub use cursor_send::CursorDisableReason;",
                            "#[allow(unused_imports)]\n    pub use cursor_send::CursorDisableReason;")
        main = main.replace("// DIRECT_LISTENER_REASON_REEXPORT",
                            "pub use media_diagnostics::DirectListenerStopReason;")
        main += r'''fn main() {
    const MAX_SAFE: u64 = 9_007_199_254_740_991;
    for (stream_id, retired_active) in [
        (7, true), (8, false), (MAX_SAFE, true), (u64::MAX, true),
    ] {
        print!("{}", diagnostics::producer_line(runtime::HostMediaDiagnostic::DirectListenerStopped {
            stream_id, retired_active,
            reason: runtime::DirectListenerStopReason::ListenerClosed,
        }));
    }
}
'''
    else:
        main += r'''fn main() {
    const MAX_SAFE: u64 = 9_007_199_254_740_991;
    for (stream_id, send_attempts, send_errors, disabled) in [
        (7, 2, 1, true), (8, 5, 2, false),
        (MAX_SAFE, MAX_SAFE, MAX_SAFE, true),
        (u64::MAX, u64::MAX, u64::MAX, true),
    ] {
        print!("{}", diagnostics::producer_line(runtime::HostMediaDiagnostic::CursorSend {
            stream_id, send_attempts, send_errors, disabled,
            reason: runtime::CursorDisableReason::CapabilityOrSize,
        }));
    }
}
'''
    (scratch / "src").mkdir(exist_ok=True)
    (scratch / "src/main.rs").write_text(main, encoding="utf8", newline="\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event", choices=("cursor", "direct-listener"), default="cursor")
    parser.add_argument("--producer-commit")
    parser.add_argument("--scratch", type=Path)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    listener = args.event == "direct-listener"
    producer = args.producer_commit or (LISTENER_PRODUCER if listener else PRODUCER)
    contract = "listener" if listener else "cursor"
    args.scratch = args.scratch or Path(r"E:\desk-link\.worktrees\dl2-targets\dl2-services") / f"{contract}-contract"
    args.output = args.output or Path(__file__).resolve().parents[1] / f"fixtures/host-media-{contract}-producer.json"
    commit = command("git", "rev-parse", "--verify", producer + "^{commit}").decode().strip()
    sources = {path: command("git", "show", f"{commit}:{path}") for path in SOURCE_PATHS}
    scratch = args.scratch.resolve()
    marker = scratch / ".host-media-contract-scratch"
    if scratch.exists() and any(scratch.iterdir()) and not marker.is_file():
        raise RuntimeError("refusing to overwrite an unmarked nonempty scratch directory")
    scratch.mkdir(parents=True, exist_ok=True)
    marker.write_text("Owned by generate-host-media-contract.py; source snapshots are scratch only.\n", encoding="utf8", newline="\n")
    crate_files(scratch, sources, args.event)
    toolchain = tomllib.loads(sources["rust-toolchain.toml"].decode())["toolchain"]["channel"]
    env = {**os.environ, "RUSTUP_TOOLCHAIN": toolchain, "CARGO_TARGET_DIR": str(scratch / "target")}
    rustc = command("rustup", "run", toolchain, "rustc", "-Vv", env=env).decode().strip()
    wrapper = ROOT / "scripts/run-windows-cargo.py"
    invocation = [sys.executable, str(wrapper), "run", "--quiet", "--locked", "--offline", "--manifest-path", str(scratch / "Cargo.toml")]
    raw = command(*invocation, env=env)
    lines = raw.decode("utf8").splitlines(keepends=True)
    if len(lines) != 4 or any(not line.endswith("\n") or "\r" in line for line in lines):
        raise RuntimeError("producer did not emit exactly four LF JSONL records")
    if listener:
        names = ("retired_true", "retired_false", "max_safe", "u64_overflow")
        fields = ("stream_id", "retired_active")
        expectations = [(7, True), (8, False), (2**53 - 1, True), (2**64 - 1, True)]
        event_name, reason = "host_direct_listener_stopped", "listener_closed"
    else:
        names = ("disabled", "enabled", "max_safe", "u64_overflow")
        fields = ("stream_id", "send_attempts", "send_errors", "disabled")
        expectations = [(7, 2, 1, True), (8, 5, 2, False),
                        (2**53 - 1, 2**53 - 1, 2**53 - 1, True),
                        (2**64 - 1, 2**64 - 1, 2**64 - 1, True)]
        event_name, reason = "host_cursor_send", "capability_or_size"
    for line, expected in zip(lines, expectations, strict=True):
        value = json.loads(line)
        actual = tuple(value[field] for field in fields)
        if (actual != expected or value["event"] != event_name or value["reason"] != reason
                or value["schema"] != 1 or value["level"] != "warning"):
            raise RuntimeError("unexpected production encoder output")
    fixture = {
        "producer_commit": commit,
        "sources": [{"path": path,
                     "git_blob": command("git", "rev-parse", f"{commit}:{path}").decode().strip(),
                     "sha256": hashlib.sha256(contents).hexdigest()}
                    for path, contents in sources.items()],
        "rustc_version": rustc,
        "cases": [{"name": name, "line": line} for name, line in
                  zip(names, lines, strict=True)],
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(fixture, ensure_ascii=False, indent=2) + "\n", encoding="utf8", newline="\n")
    print(f"Generated {args.output} from {commit}: four original-encoder JSONL records ({rustc.splitlines()[0]}).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
