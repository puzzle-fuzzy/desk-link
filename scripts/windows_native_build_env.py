"""Prepare deterministic native build tools for DeskLink's Windows crates."""

from __future__ import annotations

import os
import shutil
from pathlib import Path


def prepare_windows_native_build_environment() -> None:
    """Put Visual Studio's bundled Ninja on PATH when none is configured."""

    if os.name != "nt" or shutil.which("ninja"):
        return

    roots = [
        Path(os.environ.get("ProgramFiles", r"C:\Program Files")),
        Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")),
    ]
    suffix = Path("Common7/IDE/CommonExtensions/Microsoft/CMake/Ninja/ninja.exe")
    candidates: list[Path] = []
    for root in roots:
        visual_studio = root / "Microsoft Visual Studio"
        if not visual_studio.is_dir():
            continue
        for version in visual_studio.iterdir():
            if not version.is_dir():
                continue
            for edition in version.iterdir():
                candidate = edition / suffix
                if candidate.is_file():
                    candidates.append(candidate)

    if not candidates:
        raise SystemExit(
            "DeskLink needs Ninja to build its vendored Windows audio codec. "
            "Install the Visual Studio C++ CMake tools component or put ninja.exe on PATH."
        )

    selected = sorted(candidates, reverse=True)[0]
    os.environ["PATH"] = str(selected.parent) + os.pathsep + os.environ.get("PATH", "")


def prepare_windows_release_environment() -> None:
    """Make release binaries independent from the Visual C++ redistributable."""

    if os.name != "nt":
        return
    rustflags = os.environ.get("RUSTFLAGS", "").strip()
    if "target-feature=-crt-static" in rustflags:
        raise SystemExit(
            "DeskLink Windows releases require the static MSVC runtime; "
            "remove target-feature=-crt-static from RUSTFLAGS."
        )
    if "target-feature=+crt-static" not in rustflags:
        os.environ["RUSTFLAGS"] = " ".join(
            part for part in (rustflags, "-C target-feature=+crt-static") if part
        )
