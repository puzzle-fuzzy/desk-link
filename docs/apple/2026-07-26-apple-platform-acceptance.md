# Apple platform acceptance summary

Date: 2026-07-26

This document separates repeatable automated evidence from interactive acceptance. Simulator and unsigned device builds prove packaging and compilation; they do not prove Apple permission prompts, code signing, a physical iPhone, or a second Mac.

## Automated evidence

- [x] `cargo fmt --all -- --check`
- [x] `cargo test --workspace`
- [x] `cargo test -p desklink-ffi`
- [x] `cargo test --manifest-path tests/end-to-end/Cargo.toml`
- [x] `(cd apps/apple && swift test --arch arm64)`
- [x] `./scripts/verify-macos-runtime.sh`
- [x] `./scripts/verify-ios.sh`
- [x] iOS simulator: 11 unit tests and 1 UI test passed after lifecycle coverage was added.
- [x] iOS device target: unsigned generic `iphoneos` build passed.
- [ ] Signed macOS bundle verification with `APPLE_SIGNING_IDENTITY`.
- [ ] Signed iOS archive/install with a development or distribution team.

## Scope proven by source and tests

- macOS target is Apple Silicon and retains controller plus host roles.
- iOS target is controller-only and declares `Platform::IOS` through the Rust FFI boundary.
- Pairing, saved-host Keychain records, H.264 decode, Metal presentation, direct touch, trackpad, keyboard and lifecycle stale-stream guards have automated coverage.
- iOS does not advertise Windows-specific Direct LAN probing; its first video path is relay-only.

## Interactive acceptance remaining

- [ ] macOS Screen Recording and Accessibility prompts, same-Mac host/controller loopback, approval-before-capture, input and `ReleaseAll`.
- [ ] Windows controller → macOS host and macOS controller → Windows host.
- [ ] macOS → macOS with a second physical Mac, including permission revocation and saved-host reconnect.
- [ ] Windows/macOS host → physical iPhone controller, including QR, video, touch, keyboard, network loss and background recovery.
- [ ] Physical iPhone rotation, safe-area, Keychain relaunch and low-memory behavior.
- [ ] macOS notarization and iOS signing/install evidence.

Until these rows are recorded with device, OS, build, relay mode and permission state, Apple work should be described as development-complete and automatically verified, not production-accepted.
