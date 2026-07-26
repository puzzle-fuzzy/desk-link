#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SOURCE="$ROOT/apps/apple/Resources/DeskLinkIcon.svg"
IOS_ICON="$ROOT/apps/ios/DeskLinkIOS/Assets.xcassets/AppIcon.appiconset/AppIcon-1024.png"
MAC_RESOURCES="$ROOT/apps/macos/Resources"
ICONSET_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/desklink-iconset.XXXXXX")
ICONSET="$ICONSET_ROOT.iconset"
mv "$ICONSET_ROOT" "$ICONSET"
trap 'rm -rf "$ICONSET"' EXIT HUP INT TERM

mkdir -p "$MAC_RESOURCES"
xcrun swift "$ROOT/scripts/render-apple-icon.swift" "$SOURCE" "$IOS_ICON" 1024

for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$IOS_ICON" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
done
sips -z 32 32 "$IOS_ICON" --out "$ICONSET/icon_16x16@2x.png" >/dev/null
sips -z 64 64 "$IOS_ICON" --out "$ICONSET/icon_32x32@2x.png" >/dev/null
sips -z 256 256 "$IOS_ICON" --out "$ICONSET/icon_128x128@2x.png" >/dev/null
sips -z 512 512 "$IOS_ICON" --out "$ICONSET/icon_256x256@2x.png" >/dev/null
sips -z 1024 1024 "$IOS_ICON" --out "$ICONSET/icon_512x512@2x.png" >/dev/null
iconutil -c icns -o "$MAC_RESOURCES/AppIcon.icns" "$ICONSET"
