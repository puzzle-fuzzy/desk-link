---
name: DeskLink
description: An editorial Windows control surface for a private personal remote desktop.
colors:
  primary: "#0c38b5"
  background: "#f3f2ee"
  surface: "#fffefa"
  ink: "#12130f"
  muted: "#656760"
  border: "#d4d3cd"
  success: "oklch(0.530 0.140 145)"
  info: "oklch(0.430 0.105 225)"
  error: "oklch(0.490 0.180 25)"
  on-primary: "#fffefa"
typography:
  headline:
    fontFamily: "Segoe UI Variable Text, Segoe UI, sans-serif"
    fontSize: "clamp(42px, 6vw, 66px)"
    fontWeight: 300
    lineHeight: 0.98
  title:
    fontFamily: "Segoe UI Variable Text, Segoe UI, sans-serif"
    fontSize: "24px"
    fontWeight: 400
    lineHeight: 1.35
  body:
    fontFamily: "Segoe UI Variable Text, Segoe UI, sans-serif"
    fontSize: "14px"
    fontWeight: 400
    lineHeight: 1.6
  label:
    fontFamily: "Cascadia Mono, Consolas, monospace"
    fontSize: "12px"
    fontWeight: 600
    lineHeight: 1.35
rounded:
  sm: "0px"
  md: "0px"
  lg: "0px"
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

**Creative North Star: "The Quiet Control Light"**

DeskLink should resemble one clear indicator on a well-made physical device: easy to find, unambiguous when it changes, and otherwise silent. The Windows surface uses warm paper, ink-black type, one blueprint-blue action color, remote-task-first navigation, and semantic status colors only where state needs emphasis.

This is a compact personal tool, not an enterprise console or a neon streaming overlay. Information density is moderate, controls retain native platform behavior, and security consequences are written in full Chinese sentences.

**Key Characteristics:**

- Remote-task-first information architecture with native Windows behavior
- Editorial board grammar: mono metadata, hairline rules, and numbered blocks
- One dominant action color with explicit semantic states
- Compact trusted-device management without nested navigation
- Motion only for state transitions and never for decoration

## Implementation boundary

The Windows control workspace, host action dock, connection settings, and trusted-device views are implemented as a Tauri 2 control surface using semantic HTML/CSS and Vanilla TypeScript. Rust remains the trust boundary: it owns DPAPI storage, validates all connection input, never returns the saved relay key, and exposes only the minimum Tauri commands and capabilities required by the view.

The Tauri process owns the single-instance application lifetime, native tray, and host supervisor start/stop boundary. Capture, encoding, encrypted transport, input injection, and high-consequence approval or revocation confirmations remain in Rust/Win32. The WebView receives sanitized lifecycle summaries and is a presentation layer, not a replacement for native security or media boundaries.

The current release target is Windows only. The macOS source tree is parked research code and is not part of the release gate; if cross-platform work resumes, it must first adopt this Windows information architecture and its semantic tokens instead of creating a second product surface.

## Information Architecture

The remote-task-first hierarchy is ordered by the user's next remote action:

连接设备 -> 最近设备 -> 共享此设备 -> 已批准设备 -> 设置 / 诊断

“连接设备” is the default entry and contains the recent-device reconnect path. “共享此设备” is the desktop host flow for generating an invitation and approving or revoking controllers. “已批准设备” manages trusted devices, while “设置 / 诊断” contains permissions, host availability, and technical details. Diagnostics and local runtime metrics remain secondary and must not displace the connection task.

There is no mobile release surface. Do not add mobile navigation or a second connection flow until the Windows release has completed real two-machine acceptance.

## Colors

Warm paper keeps the control surface neutral; blueprint blue is reserved for the primary action and product identity. Green, blue, and red communicate healthy, transitional, and stopped states alongside text and icons.

### Primary

- **Blueprint Blue:** used for the tray identity, focused primary action, and the single most important enabled command.

### Semantic accents

- **Recovery Blue:** used for connecting and retrying state indicators when the primary action is not already blue.

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

- **Shape:** square editorial control (0px radius) with a visible Windows focus rectangle.
- **Primary:** Blueprint Blue with near-white text and 7px by 14px padding.
- **Hover / Focus:** use the platform focus rectangle and a modest tonal shift; never scale or bounce.
- **Secondary:** Quiet Surface with Warm Ink; destructive actions stay secondary until a specific device is selected.

### Cards / Containers

- **Corner Style:** square grouping with hairline dividers, not a grid of floating cards.
- **Background:** Host White for the page and Quiet Surface for a single status or device region.
- **Shadow Strategy:** none inside the window.
- **Border:** Soft Divider only where grouping is not otherwise clear.
- **Internal Padding:** 16px for compact groups, 24px for the primary status region.

### Inputs / Fields

- **Style:** semantic HTML controls sized to Windows system metrics inside the Tauri/WebView surface.
- **Focus:** visible `:focus-visible` state and logical keyboard traversal.
- **Error / Disabled:** written explanation plus the native disabled state; color alone is forbidden.

### Navigation

The tray menu contains “Open DeskLink” and “Exit DeskLink.” The main window has three primary destinations: “控制其他电脑”, “访问管理”, and “设置”. Controlling another computer is the default. Host availability, device ID, password actions, and connection settings live in a compact dock above that workspace instead of a “本机状态” tab. Pairing, fixed password, and connection settings remain complete secondary pages opened from the dock. Closing the window returns it to the tray; only “Exit DeskLink” stops the host.

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
