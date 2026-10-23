# Product

## Register

product

## Users

DeskLink serves one person connecting their own Windows and Mac devices. The primary interaction is a quick status check, followed by an occasional high-consequence action such as approving a new controller, disconnecting a live session, or revoking a trusted device.

## Product Purpose

DeskLink provides a dependable, private remote-desktop path between personal devices. Success means the owner can understand whether the host is available or controlled, recover from ordinary network disruption, and stop or revoke access immediately without learning networking or cryptographic terminology.

## Application Account

DeskLink can be used immediately in local mode without an email account. An optional application account adds email verification, session management and account-device visibility; it does not grant remote-device access, replace host approval, or synchronize saved connections between devices. A single account may be used on multiple Windows, macOS, and iPhone installations, while each installation must pair with hosts independently. Logging out releases the current remote session, clears the installation's saved remote connection records, and removes its account session; users can re-enter local mode without an account.

## Brand Personality

Calm, precise, trustworthy. Windows and macOS share task vocabulary and a Chinese interface while preserving native platform behavior. Windows uses a clean, light desktop-tool interface with narrow navigation, a single green or teal accent, and clear hierarchy. Security information stays explicit and legible, while routine background operation stays unobtrusive. System dark mode and high-contrast settings remain supported.

## Windows Task Structure

“连接” and “共享” are the two primary entry points. Connection opens by default and centers on a device ID, an access password, and one primary connect action; recent connections support repeat use as a compact secondary list. Sharing centers on this device's ID, availability, and the controls needed to enable access and manage its password.

Approved devices, settings, diagnostics, about, and the optional account remain available with less visual weight. Each page has one task title and one primary focus. Keep copy to labels, necessary state, actionable recovery, and security decisions; avoid decorative panels, promotional descriptions, repeated headings, gradients, and visual effects. Exact colors, spacing, and component details are defined by the current Windows UI styles and [DESIGN.md](DESIGN.md).

## Anti-references

- Enterprise administration consoles with dense navigation and organizational features irrelevant to one person.
- Neon gaming or streaming overlays that make a security-sensitive host look recreational or noisy.
- Decorative security dashboards with meaningless gauges, glowing maps, or fear-driven warnings.
- Hidden background behavior, ambiguous connection states, and irreversible actions labeled only “OK” or “Yes.”

## Design Principles

1. Make connection and sharing immediate: foreground the current task and make it clear whether this desktop is available or controlled.
2. Make dangerous actions exact: identify the device and describe the consequence before approval, disconnect, or revocation.
3. Stay quiet when healthy: background availability belongs in the platform status area, not in a permanently intrusive window.
4. Explain recovery honestly: distinguish connecting, connected, retrying, expired, and permanently stopped states.
5. Preserve platform familiarity: share task structure and terminology while using native Windows and macOS behavior, keyboard access, system metrics, and standard interaction patterns.

## Accessibility & Inclusion

Target WCAG 2.2 AA-equivalent contrast and interaction clarity for custom-drawn content. Respect Windows DPI scaling and high-contrast themes plus macOS accessibility settings, keyboard navigation, screen-reader labels, and reduced-motion preferences. Never use color as the only status signal, and never require recognition of a truncated device fingerprint for a security decision.
