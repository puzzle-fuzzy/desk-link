# Task 6 report: macOS host and controller role flow

## Delivered

- Added an app-level role picker and foreground-only host/controller routing. Changing roles, closing the window, and app termination release controller input and stop both runtimes.
- Added `HostBridge`, with owned callback context/host handle lifecycle, Keychain-backed host identity, explicit invitation creation, opaque invitation clipboard transfer, permission-gated capture/input, approval/reject/trust/revoke actions, native input injection, keyframe forwarding, and capture teardown before host shutdown.
- Added host permission cards, invitation controls, approval confirmation UI, trusted-controller management, and redacted host diagnostics. The invitation material is never rendered as UI text.
- Reworked the controller flow around clipboard invitation paste, saved-host connect/reconnect, local verification-key display, safe errors, and a `SessionInputView` overlay above `MetalVideoView`.
- Expanded session and diagnostics states for connected/reconnecting/recovering/frozen presentation, frame/drop counts, stream ID, error category, keyframe recovery, and all input-release exit paths.

## TDD evidence

1. Added `HostBridgeTests.testHostBridgeStartsSafeAndApprovalIsNotImplicit` before `HostBridge` existed, including the brief's exact assertions.
2. Ran `swift test --arch arm64 --filter HostBridgeTests`; it failed at compile time because `HostBridge` was absent.
3. Implemented the bridge, role flow, and permission gates; the focused test passed.
4. The test also verifies denied permissions disable both capture and host-side input.

## Verification

```sh
cd apps/macos && swift test --arch arm64 --filter HostBridgeTests
cd apps/macos && swift test --arch arm64
git diff --check
```

Result: focused host test passed; full Swift suite passed with 29 tests and 0 failures; diff check passed.

## Concern

The linked Rust FFI release archive still emits the existing deployment-target warnings (built for macOS 26.5 while the Swift package targets macOS 13/14). No Rust or C ABI files were changed. Also, the protected Task 5 `ControllerBridge` does not expose its current decoder config version, so diagnostics deliberately shows that config ID as not announced instead of fabricating one.

## Final review fixes

- Permission checks now gate both capture and input, refresh when the app becomes active, and actively stop a connected host when Screen Recording or Accessibility is revoked.
- Revoking the active trusted controller now stops the host runtime; terminal controller errors disconnect and clear decoder/display/input state.
- Host and controller callbacks carry lifecycle generations, preventing queued callbacks from a stopped runtime from changing UI state after teardown.
- Application termination releases controller input and disconnects the controller before awaiting host capture/encoder/runtime cleanup.
- Host stop now invalidates callbacks, disables input, releases local input, and enters `.stopping` synchronously; active-controller revocation waits for shutdown before deleting its trust record.
- Approval UI now shows the non-secret controller device ID, fingerprint, and current invitation expiry.
- The production host startup path now creates its initial QUIC client inside the persistent host worker runtime, so the inbound transport reader remains alive after startup returns.
