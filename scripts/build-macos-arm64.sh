#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
CHECK_ONLY=0
case "${1:-}" in
    "") ;;
    --check) CHECK_ONLY=1 ;;
    *)
        echo "usage: $0 [--check]" >&2
        exit 64
        ;;
esac

MACOSX_DEPLOYMENT_TARGET=13.0 \
    cargo build --manifest-path "$ROOT/Cargo.toml" --release -p desklink-ffi --target aarch64-apple-darwin

cd "$ROOT/apps/macos"
swift build -c release --arch arm64 \
    -Xlinker -L"$ROOT/target/aarch64-apple-darwin/release" \
    -Xlinker -ldesklink_ffi

APP="$ROOT/dist/macos/DeskLink.app"
EXECUTABLE="$ROOT/apps/macos/.build/arm64-apple-macosx/release/DeskLinkApp"
RUST_LIBRARY="$ROOT/target/aarch64-apple-darwin/release/libdesklink_ffi.a"

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$EXECUTABLE" "$APP/Contents/MacOS/DeskLinkApp"
cp "$ROOT/apps/macos/Info.plist" "$APP/Contents/Info.plist"

if [ "$CHECK_ONLY" -eq 1 ]; then
    test -f "$RUST_LIBRARY"
    test "$(lipo -archs "$RUST_LIBRARY")" = 'arm64'
    test -x "$APP/Contents/MacOS/DeskLinkApp"
    test "$(lipo -archs "$APP/Contents/MacOS/DeskLinkApp")" = 'arm64'
    /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist" | grep -qx 'com.desklink.desktop'
    /usr/libexec/PlistBuddy -c 'Print :NSScreenCaptureUsageDescription' "$APP/Contents/Info.plist" >/dev/null
    /usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$APP/Contents/Info.plist" | grep -qx '13.0'
fi
