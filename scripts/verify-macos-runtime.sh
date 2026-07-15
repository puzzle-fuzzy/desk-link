#!/bin/sh
set -eu

cargo fmt --all -- --check
cargo test -p desklink-session --test state_machine
cargo test -p desklink-ffi
cargo test --manifest-path tests/end-to-end/Cargo.toml
./scripts/build-macos-arm64.sh --check
(cd apps/macos && swift test --arch arm64)
