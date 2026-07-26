#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
DEVICE_TARGET=aarch64-apple-ios
SIMULATOR_TARGET=aarch64-apple-ios-sim
OUTPUT="$ROOT/dist/apple/DeskLinkFFI.xcframework"
HEADER_DIR="$ROOT/crates/desklink-ffi/include"

export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-16.0}"

rustup target add "$DEVICE_TARGET" "$SIMULATOR_TARGET"

MACOSX_DEPLOYMENT_TARGET=13.0 \
    cargo build --manifest-path "$ROOT/Cargo.toml" --release -p desklink-ffi --target "$DEVICE_TARGET"
MACOSX_DEPLOYMENT_TARGET=13.0 \
    cargo build --manifest-path "$ROOT/Cargo.toml" --release -p desklink-ffi --target "$SIMULATOR_TARGET"

DEVICE_LIBRARY="$ROOT/target/$DEVICE_TARGET/release/libdesklink_ffi.a"
SIMULATOR_LIBRARY="$ROOT/target/$SIMULATOR_TARGET/release/libdesklink_ffi.a"
test -f "$DEVICE_LIBRARY"
test -f "$SIMULATOR_LIBRARY"
test "$(lipo -archs "$DEVICE_LIBRARY")" = 'arm64'
test "$(lipo -archs "$SIMULATOR_LIBRARY")" = 'arm64'
cmp -s "$HEADER_DIR/desklink.h" "$ROOT/apps/apple/Sources/DeskLinkC/include/desklink.h"

rm -rf "$OUTPUT"
mkdir -p "$(dirname -- "$OUTPUT")"
xcodebuild -create-xcframework \
    -library "$DEVICE_LIBRARY" \
    -headers "$HEADER_DIR" \
    -library "$SIMULATOR_LIBRARY" \
    -headers "$HEADER_DIR" \
    -output "$OUTPUT"

test -f "$OUTPUT/ios-arm64/Headers/desklink.h"
test -f "$OUTPUT/ios-arm64-simulator/Headers/desklink.h"
cmp -s "$HEADER_DIR/desklink.h" "$OUTPUT/ios-arm64/Headers/desklink.h"
cmp -s "$HEADER_DIR/desklink.h" "$OUTPUT/ios-arm64-simulator/Headers/desklink.h"
