# macOS Apple Silicon acceptance

This record separates automated evidence from permissioned, interactive acceptance. It must not mark a physical second-Mac test complete when only a same-Mac loopback was performed.

## Environment

- Date: 2026-07-26
- Host: Apple Silicon macOS workstation
- Xcode / Swift: Xcode 26.6 / Swift 6.3.3
- Rust target: `aarch64-apple-darwin`
- Relay mode: local relay for automated tests; loopback relay for manual checks

## Automated evidence

- [x] `cargo test -p desklink-ffi`
- [x] `cargo test --manifest-path tests/end-to-end/Cargo.toml`
- [x] `(cd apps/apple && swift test --arch arm64)`
- [x] `(cd apps/macos && swift test --arch arm64)`
- [x] `./scripts/build-macos-arm64.sh --check`
- [ ] Signed bundle verification with `APPLE_SIGNING_IDENTITY` (run when signing the release artifact)

## Same-Mac loopback

Run `./scripts/launch-macos-loopback.sh` after starting the local relay. Record each result here rather than inferring it from compilation:

- [ ] Host asks for Screen Recording and Accessibility separately, with actionable System Settings links.
- [ ] One-time invite: rejection, fresh invite, approval, and saved-host persistence.
- [ ] No capture, `VideoConfig`, video frame, or input injection before approval.
- [ ] H.264 display, visible-corner pointer mapping, click/drag/scroll, modifiers, and Chinese text.
- [ ] Relay interruption, reconnect/recovery, and keyframe request.
- [ ] Controller close and host termination release all pressed input.
- [ ] Saved-host reconnect and trusted-controller revocation reject the old record.

## Cross-device acceptance

- [ ] macOS controller → Windows host.
- [ ] Windows controller → macOS host.
- [ ] Physical second-Mac host/controller test.

Unresolved items remain open until the corresponding interactive test is run and recorded with its permission state and relay configuration.
