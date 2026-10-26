#!/bin/sh
# Sourced by Apple build/check scripts. CMake builds the vendored static Opus
# sources; it is a build prerequisite and is never included in the app bundle.
if [ -z "${CMAKE:-}" ]; then
    if command -v cmake >/dev/null 2>&1; then
        CMAKE=$(command -v cmake)
    else
        DESKLINK_BUILD_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
        CMAKE="$DESKLINK_BUILD_ROOT/.tools/cmake/cmake/data/bin/cmake"
    fi
fi
if [ ! -x "$CMAKE" ]; then
    echo 'DeskLink Apple builds require CMake for the vendored Opus codec. Install CMake or set CMAKE to its executable.' >&2
    exit 1
fi
export CMAKE
