#!/bin/sh
set -eu

cargo fmt --all -- --check
cargo test -p desklink-ffi
cargo test --manifest-path tests/end-to-end/Cargo.toml
(cd apps/macos && swift test --arch arm64)
./scripts/build-macos-arm64.sh --check
