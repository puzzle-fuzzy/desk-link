---
name: DeskLink
description: A RemoteFlow-inspired Windows control surface for a private personal remote desktop.
colors:
  primary: "#2563eb"
  background: "#f9f9ff"
  surface: "#ffffff"
  ink: "#111c2d"
  muted: "#64748b"
  border: "#e2e8f0"
  success: "#15803d"
  info: "#004ac6"
  error: "#ba1a1a"
  on-primary: "#ffffff"
typography:
  headline:
    fontFamily: "Inter, v-sans, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"
    fontSize: "30px"
    fontWeight: 600
    lineHeight: 38px
  title:
    fontFamily: "Inter, v-sans, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"
    fontSize: "20px"
    fontWeight: 600
    lineHeight: 28px
  body:
    fontFamily: "Inter, v-sans, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 20px
  label:
    fontFamily: "Inter, v-sans, system-ui, -apple-system, BlinkMacSystemFont, Segoe UI, sans-serif"
    fontSize: "14px"
    fontWeight: 500
    lineHeight: 20px
rounded:
  sm: "4px"
  md: "8px"
  lg: "8px"
spacing:
  xs: "4px"
  sm: "8px"
  md: "16px"
  lg: "24px"
  xl: "32px"
components:
  button-primary:
    backgroundColor: "{colors.primary}"
    textColor: "{colors.on-primary}"
    typography: "{typography.body}"
    rounded: "{rounded.sm}"
    padding: "7px 14px"
  button-secondary:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.sm}"
    padding: "7px 14px"
  status-window:
    backgroundColor: "{colors.background}"
    textColor: "{colors.ink}"
    rounded: "{rounded.md}"
    padding: "24px"
---

# Design System: DeskLink

## Overview

**Product North Star: "RemoteFlow：让远程内容成为主角"**

DeskLink should make the next connection obvious within one glance. The Windows surface follows the supplied RemoteFlow controller reference: a quiet left navigation rail, a compact workspace context bar, near-white canvas, Inter with the existing Chinese system fallback, one blue primary action, 8px component geometry and low-contrast outlines. Shared-device management, approved devices, settings and diagnostics remain complete but visually secondary to the connection workspace.

This is a compact personal tool, not an enterprise console or a neon streaming overlay. Information density is moderate, controls retain native platform behavior, and security consequences are written in full Chinese sentences.

**Key Characteristics:**

- Remote-task-first information architecture with native Windows behavior
- Clear primary / secondary / tertiary action hierarchy
- One dominant action color with explicit semantic states
- 4px spacing rhythm with 16px functional gutters and 32px desktop margins
- Flat cards with low-contrast outlines; only active menus may use restrained elevation
- Compact sidebar navigation with a small More menu for tertiary links
- Native Windows title bar and window controls; no custom titlebar chrome
- Motion only for state transitions and never for decoration

## Implementation boundary

The Windows control workspace, host action dock, connection settings, and trusted-device views are implemented as a Tauri 2 control surface using semantic HTML/CSS and Vanilla TypeScript. Rust remains the trust boundary: it owns DPAPI storage, validates all connection input, never returns the saved relay key, and exposes only the minimum Tauri commands and capabilities required by the view.

The Tauri process owns the single-instance application lifetime, native tray, and host supervisor start/stop boundary. Capture, encoding, encrypted transport, input injection, and high-consequence approval or revocation confirmations remain in Rust/Win32. The WebView receives sanitized lifecycle summaries and is a presentation layer, not a replacement for native security or media boundaries.

The current production release target remains Windows. The Apple development surface is now active but has its own native shell: macOS is an Apple Silicon controller/host, while iOS is a controller-only client. Apple targets share security and media models through `DeskLinkAppleCore`, but their permission prompts, navigation, touch input and lifecycle behavior remain platform-native. Apple builds have separate gates and do not widen the Windows release promise.

## Information Architecture

The remote-task-first hierarchy is ordered by the user's next remote action:

连接设备 -> 最近设备 -> 共享此设备 -> 已批准设备 -> 设置 / 诊断

“连接设备” is the default entry and contains the recent-device reconnect path. “共享此设备” is the desktop host flow for generating an invitation and approving or revoking controllers. “已批准设备” manages trusted devices, while “设置 / 诊断” contains permissions, host availability, and technical details. Diagnostics and local runtime metrics remain secondary and must not displace the connection task.

The iOS surface is intentionally controller-only. Its primary navigation is connection, saved devices and secondary diagnostics; it must not expose “共享此设备”, host approval, screen capture or any implication that iOS can control another iOS app. A remote session uses the native iOS video/input surface, with background recovery treated as a security and resource lifecycle rather than a second navigation flow.

### Apple surface boundary

- **Shared core:** Keychain identity and saved-host records, Rust FFI bridge, H.264 decode, stream freshness and pure input commands.
- **macOS host/controller:** ScreenCaptureKit capture, VideoToolbox encode, CGEvent injection, native permission guidance and host approval.
- **iOS controller:** Metal display, direct-touch/trackpad mapping, committed Unicode and special-key input, QR pairing and scene-phase suspend/resume.

The shared core never creates a QUIC socket in Swift and never moves private keys or relay authentication into UI state, logs or `UserDefaults`.

## Colors

Near-white keeps the control surface neutral; #2563eb is reserved for primary actions, active states and focus indicators. Green, blue, and red communicate healthy, transitional, and stopped states alongside text and icons.

### Primary

- **RemoteFlow Blue:** used for the tray identity, focused primary action, and the single most important enabled command.

### Semantic accents

- **Recovery Blue:** used for connecting and retrying state indicators when the primary action is not already blue.

### Neutral

- **Host White:** the main window background.
- **Quiet Surface:** toolbar, list, and read-only status grouping.
- **Slate Ink:** primary text with high contrast.
- **Muted Slate:** secondary timestamps and explanatory text.
- **Soft Divider:** structural separation only.

**The One Signal Rule.** At most one saturated status or action color dominates a view. Status is always paired with a written label.

## Typography

**Windows Font:** `Inter, v-sans, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif` (with emoji fallbacks)
**macOS Font:** SF Pro through SwiftUI system typography

**Character:** familiar, compact, and highly legible at desktop scale. Weight and spacing create hierarchy without introducing a second typeface on either platform.

### Hierarchy

- **Headline** (600, 30px, 38px): window title and current connection state.
- **Title** (600, 20px, 28px): trusted-controller group and high-consequence confirmation title.
- **Body** (400, 14px, 20px): status detail, device identity, and consequences; prose stays below 70 characters per line where possible.
- **Label** (500, 14px, 20px): field labels and compact metadata, in sentence case.

**The Plain Label Rule.** Buttons name the action and object, such as “Revoke controller” and “Exit DeskLink.” Never use an unexplained “OK” for a security action.

## Elevation

The system is flat by default. Native window elevation comes from Windows itself; internal depth uses tonal surfaces and dividers rather than decorative shadows.

**The System Owns the Shadow Rule.** Never draw additional card shadows inside the native window.

## Components

### Buttons

- **Shape:** disciplined 8px product control with a visible Windows focus rectangle.
- **Primary:** RemoteFlow Blue with white text and 10px by 16px padding.
- **Hover / Focus:** use the platform focus rectangle and a modest tonal shift; never scale or bounce.
- **Secondary:** Quiet Surface with Warm Ink; destructive actions stay secondary until a specific device is selected.

### Cards / Containers

- **Corner Style:** 8px rounded grouping with a single light border.
- **Background:** Host White for the page and Quiet Surface for secondary device regions.
- **Shadow Strategy:** none; use the border and a small surface-color change to communicate grouping.
- **Border:** Soft Divider only where grouping is not otherwise clear.
- **Internal Padding:** 16px for compact groups, 24–36px for the primary connection region.

### Inputs / Fields

- **Style:** semantic HTML controls sized to Windows system metrics inside the Tauri/WebView surface.
- **Focus:** visible `:focus-visible` state and logical keyboard traversal.
- **Error / Disabled:** written explanation plus the native disabled state; color alone is forbidden.

### Navigation

The tray menu contains “Open DeskLink” and “Exit DeskLink.” The main window uses the native Windows title bar and a RemoteFlow-style left rail. “连接设备” is the primary destination; shared-device management, approved devices and settings / diagnostics are visible as secondary rail entries, while about and project links stay behind the compact “更多” menu. Closing the native window returns it to the tray; only “Exit DeskLink” stops the host.

### Pairing and Revocation

- Create pairing only after an explicit local action and require saved relay settings plus an available trusted-device store.
- Show only the public device ID, temporary password, and live expiry. The signed relay invitation stays inside Rust and the managed directory response; never expose it to the WebView or logs.
- Clear the temporary password from the WebView on cancellation, expiry, revocation restart, or pairing-worker completion; restore normal hosting after cancellation or preparation failure.
- Pairing approval and trusted-controller revocation use native Win32 Yes/No confirmations with “No” selected by default. The WebView must not imitate or replace that decision boundary.
- A successful revocation restarts the host immediately so an already-authorized runtime cannot retain access under stale trust.

### Connection Status

Show the written state, current stream when connected, retry count and delay when recovering, and the last safe error when stopped. Never expose relay authentication or private-key material.

On the healthy Windows control workspace, the host dock shows only availability, device ID, password actions, and connection settings. Relay mode, protection implementation, approved-device counts, and successful diagnostics never appear in the primary workspace. Technical relay parameters remain available only inside the secondary connection-settings page; diagnostics stay in settings unless an actionable warning must appear beside the host dock.

### Error Feedback and Diagnostics

- Keep host availability independent from trusted-controller list failures.
- Show refresh and revocation failures inline with a specific recovery action; never discard an operation error silently.
- Replace internal runtime error strings with stable owner-facing explanations. Technical detail belongs in the local structured diagnostic log.
- Keep the diagnostic log bounded and redact named credentials plus long hexadecimal identity or secret material before persistence.
- Disable destructive controls whenever no exact trusted controller is selected, including partial-load and corrupt-store states.

### Runtime Resilience

- On a multi-display Windows desktop, enumerate attached outputs, start on the Windows primary display, and let the controller switch the active captured display from the live-session toolbar. Input coordinates must be mapped through the selected display's desktop rectangle into the full Windows virtual desktop; the current build switches displays rather than compositing them.
- Register for Windows suspend/resume callbacks independently of the WebView window. Debounce duplicate resume notifications and rebuild the host supervisor so QUIC, Noise, capture, encoding, and input state are all fresh after wake.
- Resolve current-user storage through the Windows Local AppData known folder rather than depending on a process environment variable.
- Keep the repeatable Windows acceptance path in `scripts/verify-windows-resilience.py`, including physical capture, repeated relay recovery, power notification registration, and an encrypted hardware-media soak.

## Do's and Don'ts

### Do:

- **Do** keep host availability visible in the compact dock without competing with the control task.
- **Do** identify trusted controllers with their full device ID and public-key fingerprint before revocation.
- **Do** preserve keyboard access, DPI scaling, high contrast, and native focus behavior.
- **Do** keep healthy background operation in the tray and make explicit exit discoverable.

### Don't:

- **Don't** build an enterprise administration console with dense navigation and irrelevant organization features.
- **Don't** add “本机状态”, relay explanations, protection summaries, or approved-device counts back to primary navigation or the default workspace.
- **Don't** use neon gaming or streaming overlays.
- **Don't** add decorative security gauges, glowing maps, fear-driven warnings, glass surfaces, or gradient text.
- **Don't** hide background behavior or label irreversible actions only “OK” or “Yes.”
- **Don't** use color as the only signal or truncate the identity needed for a security decision.
