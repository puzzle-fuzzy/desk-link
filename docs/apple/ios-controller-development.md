# iOS controller development

DeskLink iOS is an iOS 16+ controller-only client. It controls an approved Windows or macOS host through the Rust relay/Noise boundary; it does not capture the iPhone screen, inject events into another iOS app, or expose a host/sharing mode.

## Build and test

From the repository root:

```sh
./scripts/verify-ios.sh
```

The gate builds `aarch64-apple-ios` and `aarch64-apple-ios-sim`, generates `dist/apple/DeskLinkFFI.xcframework`, runs the shared Apple package tests, builds an unsigned device application, and runs iOS simulator unit/UI tests. A passing simulator gate is not physical-device or signing acceptance.

The Xcode project is [`apps/ios/DeskLinkIOS.xcodeproj`](../../apps/ios/DeskLinkIOS.xcodeproj). Production relay values are injected through `DeskLinkRelayURL` and `DeskLinkRelayServerName`; the app does not put relay authentication or private keys in `UserDefaults`.

## Runtime boundaries

- `DeskLinkAppleCore` owns identity Keychain records, saved approved-host records, the Rust FFI callback bridge, H.264 decoding and stream freshness.
- `IOSSessionView` owns only the native session composition: Metal video, connection state, diagnostics, disconnect and keyframe actions.
- Direct touch maps only inside the aspect-fit video rectangle. Trackpad mode converts one-finger movement to bounded normalized pointer movement and two-finger movement to bounded wheel input.
- `IOSKeyboardInput` sends committed Unicode scalars and explicit special-key press/release pairs. Resigning the responder or destroying the input view calls `ReleaseAll`.
- `IOSSessionLifecycle` debounces foreground recovery, releases input on background, destroys the old native runtime, clears displayed frames, and reconnects a saved host with a fresh callback generation and stream/config boundary.

## Pairing and storage

Use the connection page to paste a pairing invite or scan its QR representation. The same strict invite decoder is used for both paths. After host approval, the validated saved-host material is written to Keychain and can be reconnected from the saved-devices tab. Secret invite fields, relay authentication and private keys are never rendered in diagnostics or logs.

## Device acceptance still required

Before calling the iOS client production-ready, install a signed build on a physical iPhone and record:

- QR camera permission, paste/manual invite and saved-host reconnect;
- Windows host and macOS host video, direct touch, trackpad, wheel, Unicode and special keys;
- background/foreground recovery, app termination, network loss and input release;
- device rotation, safe-area layout, low-memory behavior and Keychain persistence after relaunch.

The camera usage description exists for the QR scanner. No local-network permission is required for the first iOS video path; iOS uses relay video only.
