---
name: DeskLink
description: A calm cross-platform control surface for a private personal remote desktop.
colors:
  primary: "oklch(0.500 0.151 40)"
  background: "oklch(1.000 0.000 0)"
  surface: "oklch(0.965 0.006 40)"
  ink: "oklch(0.220 0.020 40)"
  muted: "oklch(0.460 0.020 40)"
  border: "oklch(0.860 0.010 40)"
  success: "oklch(0.530 0.140 145)"
  info: "oklch(0.430 0.105 225)"
  error: "oklch(0.490 0.180 25)"
  on-primary: "oklch(0.985 0.000 0)"
typography:
  headline:
    fontFamily: "Segoe UI Variable, Segoe UI, sans-serif"
    fontSize: "24px"
    fontWeight: 600
    lineHeight: 1.25
  title:
    fontFamily: "Segoe UI Variable, Segoe UI, sans-serif"
    fontSize: "16px"
    fontWeight: 600
    lineHeight: 1.35
  body:
    fontFamily: "Segoe UI Variable, Segoe UI, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.45
  label:
    fontFamily: "Segoe UI Variable, Segoe UI, sans-serif"
    fontSize: "12px"
    fontWeight: 600
    lineHeight: 1.35
rounded:
  sm: "4px"
  md: "8px"
  lg: "12px"
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
    padding: "8px 16px"
  button-secondary:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.sm}"
    padding: "8px 16px"
  status-window:
    backgroundColor: "{colors.background}"
    textColor: "{colors.ink}"
    rounded: "{rounded.md}"
    padding: "24px"
---

# Design System: DeskLink

## Overview

**Creative North Star: "The Quiet Control Light"**

DeskLink should resemble one clear indicator on a well-made physical device: easy to find, unambiguous when it changes, and otherwise silent. Windows and macOS use the same restrained white surface, one burnt-coral action color, four-section navigation, and semantic status colors only where state needs emphasis.

This is a compact personal tool, not an enterprise console or a neon streaming overlay. Information density is moderate, controls retain native platform behavior, and security consequences are written in full Chinese sentences.

**Key Characteristics:**

- Shared Windows/macOS information architecture with native platform behavior
- Status-first hierarchy with plain-language recovery detail
- Restrained color with explicit semantic states
- Compact trusted-device management without nested navigation
- No decorative motion

## Implementation boundary

The ordinary Windows status, connection, and trusted-device views are implemented as a Tauri 2 control surface using semantic HTML/CSS and Vanilla TypeScript. Rust remains the trust boundary: it owns DPAPI storage, validates all connection input, never returns the saved relay key, and exposes only the minimum Tauri commands and capabilities required by the view.

The Tauri process owns the single-instance application lifetime, native tray, and host supervisor start/stop boundary. Capture, encoding, encrypted transport, input injection, and high-consequence approval or revocation confirmations remain in Rust/Win32. The WebView receives sanitized lifecycle summaries and is a presentation layer, not a replacement for native security or media boundaries.

The macOS surface is implemented with SwiftUI and mirrors the Windows top bar, four navigation sections, status-first hierarchy, flat groups, Chinese copy, and shared semantic colors. SwiftUI owns presentation only; Keychain, Rust FFI, ScreenCaptureKit, VideoToolbox, AppKit input injection, and system permission boundaries retain their platform responsibilities.

## Colors

Pure white keeps the host surface neutral; burnt coral is reserved for the primary action and product identity. Green, blue, and red communicate healthy, transitional, and stopped states alongside text and icons.

### Primary

- **Control Coral:** used for the tray identity, focused primary action, and the single most important enabled command.

### Secondary

- **Recovery Blue:** used for connecting and retrying state indicators, never for decorative chrome.

### Neutral

- **Host White:** the main window background.
- **Quiet Surface:** toolbar, list, and read-only status grouping.
- **Warm Ink:** primary text with high contrast.
- **Muted Ink:** secondary timestamps and explanatory text.
- **Soft Divider:** structural separation only.

**The One Signal Rule.** At most one saturated status or action color dominates a view. Status is always paired with a written label.

## Typography

**Windows Font:** Segoe UI Variable (with Segoe UI fallback)
**macOS Font:** SF Pro through SwiftUI system typography

**Character:** familiar, compact, and highly legible at desktop scale. Weight and spacing create hierarchy without introducing a second typeface on either platform.

### Hierarchy

- **Headline** (600, 24px, 1.25): window title and current connection state.
- **Title** (600, 16px, 1.35): trusted-controller group and high-consequence confirmation title.
- **Body** (400, 14px, 1.45): status detail, device identity, and consequences; prose stays below 70 characters per line where possible.
- **Label** (600, 12px, 1.35): field labels and compact metadata, in sentence case.

**The Plain Label Rule.** Buttons name the action and object, such as “Revoke controller” and “Exit DeskLink.” Never use an unexplained “OK” for a security action.

## Elevation

The system is flat by default. Native window elevation comes from Windows itself; internal depth uses tonal surfaces and dividers rather than decorative shadows.

**The System Owns the Shadow Rule.** Never draw additional card shadows inside the native window.

## Components

### Buttons

- **Shape:** standard, gently curved Windows control (4px radius).
- **Primary:** Control Coral with near-white text and 8px by 16px padding.
- **Hover / Focus:** use the platform focus rectangle and a modest tonal shift; never scale or bounce.
- **Secondary:** Quiet Surface with Warm Ink; destructive actions stay secondary until a specific device is selected.

### Cards / Containers

- **Corner Style:** compact grouping only (8px radius), not a grid of floating cards.
- **Background:** Host White for the page and Quiet Surface for a single status or device region.
- **Shadow Strategy:** none inside the window.
- **Border:** Soft Divider only where grouping is not otherwise clear.
- **Internal Padding:** 16px for compact groups, 24px for the primary status region.

### Inputs / Fields

- **Style:** semantic HTML controls sized to Windows system metrics inside the Tauri/WebView surface.
- **Focus:** visible `:focus-visible` state and logical keyboard traversal.
- **Error / Disabled:** written explanation plus the native disabled state; color alone is forbidden.

### Navigation

The tray menu contains “Open DeskLink” and “Exit DeskLink.” Trusted-device management stays in the main window so the tray remains compact. The main window is a single surface, not a sidebar application. Closing the window returns it to the tray; only “Exit DeskLink” stops the host.

### Pairing and Revocation

- Create pairing only after an explicit local action and require saved relay settings plus an available trusted-device store.
- Show the single signed invitation, its live expiry, and a plain warning that it contains the private relay join secret retained by an approved controller for reconnecting. Never log it or return it as part of routine status refreshes.
- Clear the WebView copy on cancellation, expiry, revocation restart, or pairing-worker completion; restore normal hosting after cancellation or preparation failure.
- Pairing approval and trusted-controller revocation use native Win32 Yes/No confirmations with “No” selected by default. The WebView must not imitate or replace that decision boundary.
- A successful revocation restarts the host immediately so an already-authorized runtime cannot retain access under stale trust.

### Connection Status

Show the written state, current stream when connected, retry count and delay when recovering, and the last safe error when stopped. Never expose relay authentication or private-key material.

### Error Feedback and Diagnostics

- Keep host availability independent from trusted-controller list failures.
- Show refresh and revocation failures inline with a specific recovery action; never discard an operation error silently.
- Replace internal runtime error strings with stable owner-facing explanations. Technical detail belongs in the local structured diagnostic log.
- Keep the diagnostic log bounded and redact named credentials plus long hexadecimal identity or secret material before persistence.
- Disable destructive controls whenever no exact trusted controller is selected, including partial-load and corrupt-store states.

### Runtime Resilience

- On a multi-display Windows desktop, capture the attached output whose desktop coordinates contain `(0, 0)`, which is the Windows primary display. Do not imply that the current build composites the full virtual desktop or supports display switching.
- Register for Windows suspend/resume callbacks independently of the WebView window. Debounce duplicate resume notifications and rebuild the host supervisor so QUIC, Noise, capture, encoding, and input state are all fresh after wake.
- Resolve current-user storage through the Windows Local AppData known folder rather than depending on a process environment variable.
- Keep the repeatable Windows acceptance path in `scripts/verify-windows-resilience.py`, including physical capture, repeated relay recovery, power notification registration, and an encrypted hardware-media soak.

## Do's and Don'ts

### Do:

- **Do** keep the current connection state visible at the top of the window.
- **Do** identify trusted controllers with their full device ID and public-key fingerprint before revocation.
- **Do** preserve keyboard access, DPI scaling, high contrast, and native focus behavior.
- **Do** keep healthy background operation in the tray and make explicit exit discoverable.

### Don't:

- **Don't** build an enterprise administration console with dense navigation and irrelevant organization features.
- **Don't** use neon gaming or streaming overlays.
- **Don't** add decorative security gauges, glowing maps, fear-driven warnings, glass surfaces, or gradient text.
- **Don't** hide background behavior or label irreversible actions only “OK” or “Yes.”
- **Don't** use color as the only signal or truncate the identity needed for a security decision.
