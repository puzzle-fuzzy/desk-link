#!/usr/bin/env python3
"""Run Cargo with the native Windows tools DeskLink requires on PATH."""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path


SCRIPTS_DIRECTORY = Path(__file__).resolve().parent
ROOT = SCRIPTS_DIRECTORY.parent
if str(SCRIPTS_DIRECTORY) not in sys.path:
    sys.path.insert(0, str(SCRIPTS_DIRECTORY))

from windows_native_build_env import prepare_windows_native_build_environment


def main() -> int:
    if len(sys.argv) < 2:
        raise SystemExit("usage: run-windows-cargo.py <cargo arguments...>")
    prepare_windows_native_build_environment()
    subprocess.run(["cargo", *sys.argv[1:]], cwd=ROOT, check=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
