#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
APP="${DESKLINK_APP:-$ROOT/dist/macos/DeskLink.app/Contents/MacOS/DeskLinkApp}"
RELAY_URL="${DESKLINK_RELAY_URL:-quic://127.0.0.1:4433}"
RELAY_SERVER_NAME="${DESKLINK_RELAY_SERVER_NAME:-localhost}"

if [ ! -x "$APP" ]; then
    echo "DeskLink.app is missing; run ./scripts/build-macos-arm64.sh --check first." >&2
    exit 1
fi

echo "Starting two macOS instances against $RELAY_URL"
echo "Use one window as Host and the other as Controller; stop with Ctrl-C."

DESKLINK_RELAY_URL="$RELAY_URL" \
DESKLINK_RELAY_SERVER_NAME="$RELAY_SERVER_NAME" \
    "$APP" &
HOST_PID=$!
DESKLINK_RELAY_URL="$RELAY_URL" \
DESKLINK_RELAY_SERVER_NAME="$RELAY_SERVER_NAME" \
    "$APP" &
CONTROLLER_PID=$!

cleanup() {
    kill "$HOST_PID" "$CONTROLLER_PID" 2>/dev/null || true
}
trap cleanup INT TERM EXIT

wait "$HOST_PID" "$CONTROLLER_PID"
