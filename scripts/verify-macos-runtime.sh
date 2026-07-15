#!/bin/sh
set -eu

# The linked worktree's vendored package metadata resolves against the parent
# workspace under cargo fmt --all. Format the Rust FFI package that this macOS
# build owns; the full-workspace command remains documented as an environment
# limitation in the Task 7 report.
cargo fmt -p desklink-ffi -- --check
cargo test -p desklink-ffi
cargo test --manifest-path tests/end-to-end/Cargo.toml
(cd apps/macos && swift test --arch arm64)
./scripts/build-macos-arm64.sh --check
