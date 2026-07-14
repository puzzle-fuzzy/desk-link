#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
CHECK_ONLY=0
if [ "${1:-}" = "--check" ]; then
    CHECK_ONLY=1
fi

cargo build --manifest-path "$ROOT/Cargo.toml" --release -p desklink-ffi --target aarch64-apple-darwin

cd "$ROOT/apps/macos"
swift build -c release --arch arm64 \
    -Xlinker -L"$ROOT/target/aarch64-apple-darwin/release" \
    -Xlinker -ldesklink_ffi

if [ "$CHECK_ONLY" -eq 1 ]; then
    EXECUTABLE="$ROOT/apps/macos/.build/arm64-apple-macosx/release/DeskLinkApp"
    test -x "$EXECUTABLE"
    file "$EXECUTABLE" | grep -q 'arm64'
    /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$ROOT/apps/macos/Info.plist" >/dev/null
    /usr/libexec/PlistBuddy -c 'Print :NSScreenCaptureUsageDescription' "$ROOT/apps/macos/Info.plist" >/dev/null
fi
