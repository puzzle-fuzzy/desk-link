#!/usr/bin/env sh
set -eu

base_url="${DESKLINK_ACCOUNT_URL:-http://127.0.0.1:3412}"

check_status() {
  endpoint="$1"
  expected="$2"
  body="$(curl --fail --silent --show-error --max-time 10 "$base_url/$endpoint")"
  python3 - "$endpoint" "$expected" "$body" <<'PY'
import json
import sys

endpoint, expected, body = sys.argv[1:]
payload = json.loads(body)
if payload.get("status") != expected or payload.get("service") != "desklink-account":
    raise SystemExit(f"{endpoint} returned an unexpected payload: {payload}")
print(f"{endpoint}: {payload['status']}")
PY
}

check_status health ok
check_status ready ready
