"""Prepare deterministic native build tools for DeskLink's Windows crates."""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path


def msvc_environment_is_ready(environment: dict[str, str]) -> bool:
    if not shutil.which("cl", path=environment.get("PATH", "")):
        return False
    includes = [Path(value.strip().strip('"')) for value in environment.get("INCLUDE", "").split(";") if value.strip()]
    return all(any((directory / header).is_file() for directory in includes) for header in ("stddef.h", "stdint.h"))


def visual_studio_installations() -> list[Path]:
    roots = [
        Path(os.environ.get("ProgramFiles", r"C:\Program Files")),
        Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")),
    ]
    installations: list[Path] = []
    for root in roots:
        vswhere = root / "Microsoft Visual Studio/Installer/vswhere.exe"
        if not vswhere.is_file():
            continue
        try:
            result = subprocess.run(
                [str(vswhere), "-latest", "-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "installationPath", "-utf8"],
                capture_output=True,
                check=False,
                timeout=15,
            )
            if result.returncode == 0:
                installations.extend(Path(line.strip()) for line in result.stdout.decode("utf-8-sig").splitlines() if line.strip())
        except (OSError, UnicodeError, subprocess.SubprocessError):
            pass
    for root in roots:
        visual_studio = root / "Microsoft Visual Studio"
        if visual_studio.is_dir():
            installations.extend(sorted((edition for version in visual_studio.iterdir() if version.is_dir() for edition in version.iterdir() if edition.is_dir()), reverse=True))
    return list(dict.fromkeys(path.resolve() for path in installations if path.is_dir()))


def capture_msvc_environment(batch: Path) -> dict[str, str]:
    """Capture only the child environment; never print or persist its contents."""
    batch = batch.resolve()
    if any(character in str(batch) for character in '\r\n"&|<>^%!'):
        raise SystemExit("The Visual Studio developer batch path contains unsupported command characters.")
    if batch.name.lower() == "vsdevcmd.bat":
        arguments = "-no_logo -arch=x64 -host_arch=x64"
    elif batch.name.lower() == "vcvarsall.bat":
        arguments = "x64"
    else:
        raise SystemExit("Expected a Visual Studio VsDevCmd.bat or vcvarsall.bat.")
    command = f'call "{batch}" {arguments} >nul && set'
    cmd = Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32/cmd.exe"
    # cmd's /c payload is shell syntax, not C argv syntax. Passing it in an
    # argv list would turn the batch path's quotes into literal backslash-quotes.
    invocation = f'"{cmd}" /d /u /s /c "{command}"'
    try:
        result = subprocess.run(
            invocation,
            executable=str(cmd),
            capture_output=True,
            check=False,
            timeout=60,
        )
        if result.returncode != 0:
            raise SystemExit(f"Visual Studio developer environment setup failed (exit {result.returncode}).")
        output = result.stdout.decode("utf-16-le").lstrip("\ufeff")
    except (OSError, UnicodeError, subprocess.SubprocessError):
        raise SystemExit("Could not capture the Visual Studio developer environment.") from None
    environment: dict[str, str] = {}
    for line in output.splitlines():
        name, separator, value = line.partition("=")
        # cmd's hidden drive-directory entries start with '=' and are not
        # ordinary variables that Python permits setting via os.environ.
        if separator and name:
            environment[name.upper()] = value
    return environment


def prepare_msvc_environment() -> None:
    if msvc_environment_is_ready(dict(os.environ)):
        return
    for installation in visual_studio_installations():
        for suffix in ("Common7/Tools/VsDevCmd.bat", "VC/Auxiliary/Build/vcvarsall.bat"):
            batch = installation / suffix
            if not batch.is_file():
                continue
            environment = capture_msvc_environment(batch)
            if not msvc_environment_is_ready(environment):
                raise SystemExit("Visual Studio setup did not provide cl and the required C include headers. Install the MSVC C++ build tools and Windows SDK.")
            # Validate before touching the caller's environment. Existing
            # variables that the developer script did not replace stay intact.
            os.environ.update(environment)
            return
    raise SystemExit("DeskLink needs the MSVC C++ developer environment. Install Visual Studio C++ build tools with a Windows SDK, or launch from an x64 Native Tools prompt.")


def prepare_windows_native_build_environment() -> None:
    """Prepare MSVC for Cargo's C subprocesses and locate bundled Ninja."""

    if os.name != "nt":
        return
    prepare_msvc_environment()
    if shutil.which("ninja"):
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
