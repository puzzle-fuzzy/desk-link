#!/usr/bin/env python3
"""Write an offline synthetic media baseline; no desktop capture or session."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
from pathlib import Path
import subprocess

from windows_native_build_env import prepare_windows_native_build_environment

ROOT = Path(__file__).resolve().parents[1]


def source_identity(root: Path) -> dict[str, object]:
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
    dirty = bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=root, text=True).strip())
    names = subprocess.check_output(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=root).split(b"\0")
    digest = hashlib.sha256()
    for name in sorted(set(names)):
        if not name:
            continue
        path = root / os.fsdecode(name)
        if not path.is_file():
            continue
        digest.update(len(name).to_bytes(8, "little"))
        digest.update(name)
        digest.update(hashlib.sha256(path.read_bytes()).digest())
    return {"commit": commit, "dirty": dirty, "files_sha256": digest.hexdigest()}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--width", type=int, default=1920)
    parser.add_argument("--height", type=int, default=1080)
    parser.add_argument("--frames", type=int, default=120)
    parser.add_argument("--warmup", type=int, default=10)
    parser.add_argument("--encode", action="store_true", help="opt in to local synthetic MFT encoding")
    parser.add_argument("--software", action="store_true", help="with --encode, explicitly select Microsoft software MFT")
    parser.add_argument("--motion", action="store_true")
    parser.add_argument("--reuse-conversion", action="store_true", help="CPU conversion uses the reusable NV12 API")
    parser.add_argument("--profile", choices=["release", "dev"], default="release")
    parser.add_argument("--label", default="unspecified", help="non-sensitive hardware/test-condition label")
    parser.add_argument("--output", type=Path, default=ROOT / "dist/windows/media-baseline.json")
    args = parser.parse_args()
    if args.software and not args.encode:
        parser.error("--software requires --encode")
    prepare_windows_native_build_environment()
    command = ["cargo", "run", "--locked", "--profile", args.profile, "-p", "desklink-windows", "--example", "media_baseline", "--", "--width", str(args.width), "--height", str(args.height), "--frames", str(args.frames), "--warmup", str(args.warmup)]
    if args.encode:
        command.append("--encode")
    if args.software:
        command.append("--software")
    if args.motion:
        command.append("--motion")
    if args.reuse_conversion:
        command.append("--reuse-conversion")
    identity_before = source_identity(ROOT)
    result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, check=False)
    if result.returncode != 0:
        print(result.stderr)
        return result.returncode
    report = json.loads(result.stdout)
    identity_after = source_identity(ROOT)
    report["source_before"] = identity_before
    report["source_after"] = identity_after
    report["source_unchanged_during_run"] = identity_before == identity_after
    report["build_profile"] = args.profile
    report["environment"] = {"os": platform.system(), "release": platform.release(), "machine": platform.machine(), "processor": platform.processor(), "label": args.label}
    report["network"] = "none_offline"
    report["command"] = command
    report["cargo_target_dir_configured"] = "CARGO_TARGET_DIR" in os.environ
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(f"Saved offline synthetic stage baseline to {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
