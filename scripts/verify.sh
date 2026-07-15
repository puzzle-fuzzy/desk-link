#!/bin/sh
set -eu
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo test --manifest-path tests/end-to-end/Cargo.toml
(
  cd apps/windows-ui
  bun install --frozen-lockfile
  bun run test
  bun run build
)
if [ "$(uname -s)" = "Darwin" ]; then
  cargo build --release --package desklink-ffi --target aarch64-apple-darwin
  (
    cd apps/macos
    swift test
  )
fi
marker_a=$(printf '\u5f85\u5b9a')
marker_b=$(printf '\u5f85\u8865\u5145')
scan_paths="README.md docs crates server"
if [ -d tests ]; then
  scan_paths="$scan_paths tests"
fi
if rg -n "T[O][D][O]|T[B][D]|$marker_a|$marker_b" $scan_paths; then
  echo 'placeholder text found' >&2
  exit 1
fi
