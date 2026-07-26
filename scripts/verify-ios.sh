#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

"$ROOT/scripts/build-apple-rust.sh"
(cd "$ROOT/apps/apple" && swift test --arch arm64)
SIMULATOR_ID=$(xcrun simctl list devices available | sed -nE 's/.*\(([0-9A-F-]{36})\).*/\1/p' | head -n 1)
if [ -z "$SIMULATOR_ID" ]; then
    echo "error: no available iOS Simulator device" >&2
    exit 1
fi
xcodebuild \
    -project "$ROOT/apps/ios/DeskLinkIOS.xcodeproj" \
    -scheme DeskLinkIOS \
    -sdk iphoneos \
    -destination 'generic/platform=iOS' \
    CODE_SIGNING_ALLOWED=NO \
    build
xcodebuild \
    -project "$ROOT/apps/ios/DeskLinkIOS.xcodeproj" \
    -scheme DeskLinkIOS \
    -sdk iphonesimulator \
    -destination "platform=iOS Simulator,id=$SIMULATOR_ID" \
    CODE_SIGNING_ALLOWED=NO \
    test
