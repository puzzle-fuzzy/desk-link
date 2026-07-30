# DeskLink Release Architecture Contract Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 固化 DeskLink 当前 Windows 候选版、Apple 开发版、可选账号和远程连接链路的唯一事实边界，并让发布脚本在 Python 3.9 上可验证。

**Architecture:** 远程连接继续由 Rust protocol/crypto/session/transport/FFI 负责，平台代码只实现采集、编码、输入、权限和 UI；relay 只转发密文，Windows DirectLan 只承载经过认证的视频数据面并始终回落 relay。产品契约单独记录当前实现边界，原始跨平台设计文档保留为历史设计输入，不再承担发布验收标准。

**Tech Stack:** Rust 1.88/edition 2024、QUIC/TLS/Noise、Tauri 2 + TypeScript + Bun、Swift 6/Xcode、Python 3.9+ release scripts。

## Global Constraints

- Windows candidate version is `0.1.91` and the current application protocol is `12`.
- Windows release scope is Windows 10/11 x64; macOS and iOS remain development/acceptance surfaces.
- Application account is optional and never replaces pairing, host approval, or remote cryptographic authentication.
- iOS is controller-only; it must not advertise system-level iOS host control.
- DirectLan is video-only, session-bound, short-lived, and must fall back to relay without ending the control session.
- Before host approval, the host must not capture, send video configuration/frames, or inject input.
- A formal Windows release requires a clean source commit, signed artifacts, and real two-Windows acceptance.

---

### Task 1: Publish the current architecture contract

**Files:**
- Create: `docs/current-architecture-contract.md`
- Modify: `README.md`
- Modify: `docs/Windows_macOS_iOS_个人远程桌面软件详细设计与开发文档.md`

**Interfaces:**
- Consumes: current `PRODUCT.md`, `README.md`, `TODO.md`, `docs/windows-release-runbook.md`, and repository module boundaries.
- Produces: one linked contract defining platform roles, control/data planes, security invariants, release gates, and explicit non-goals.

- [x] **Step 1: Map the current release facts**

Record the exact facts: Windows `0.1.91` candidate, protocol `12`, optional account, relay-default transport, authenticated video-only DirectLan fallback, macOS host/controller development surface, and iOS controller-only surface.

- [x] **Step 2: Add the contract and cross-link the historical design**

Add `docs/current-architecture-contract.md` and a maintenance notice at the top of the original detailed design.

- [x] **Step 3: Validate the documentation boundary**

Run `rg -n "current-architecture-contract|DirectLan|controller-only|host approval" README.md PRODUCT.md TODO.md docs/current-architecture-contract.md docs/Windows_macOS_iOS_个人远程桌面软件详细设计与开发文档.md` and `git diff --check`. The current contract is linked and the historical design is explicitly scoped.

### Task 2: Keep release scripts runnable on Python 3.9

**Files:**
- Create: `scripts/cargo_manifest.py`
- Modify: `scripts/verify-windows-release.py`
- Modify: `scripts/build-windows-installer.py`
- Modify: `scripts/build-linux-relay-image.py`
- Test: `scripts/tests/test_cargo_manifest.py`

**Interfaces:**
- Consumes: a `Path` to a Cargo manifest.
- Produces: `package_version(path: Path) -> str`, using `tomllib` when available and a bounded `[package]` fallback otherwise.

- [x] **Step 1: Add the compatibility helper and focused test**

The test fixture includes a dependency section with another `version` field so the fallback is proven to read only the Cargo package section.

- [x] **Step 2: Replace direct `tomllib` imports**

Use `package_version(...)` in the Windows verification, Windows installer, and Linux relay image scripts without adding a third-party runtime dependency.

- [x] **Step 3: Run the script gates**

Run `python3 -m unittest discover -s scripts/tests -p 'test_*.py'` and `python3 -m py_compile scripts/cargo_manifest.py scripts/verify-windows-release.py scripts/build-windows-installer.py scripts/build-linux-relay-image.py`. Expected: 29 tests pass under Python 3.9.6.

### Task 3: Verify and hand off the candidate

**Files:**
- Modify: `TODO.md` only when a verified gate changes state.
- Evidence: `dist/windows/managed-relay-verification.json`, `dist/windows/windows-release-readiness.json`, and the manual acceptance record are generated artifacts.

**Interfaces:**
- Consumes: current source commit, release scripts, managed relay, managed diagnostics report, and two physical Windows devices.
- Produces: a candidate readiness report; formal release only when every P0 gate is true.

- [ ] **Step 1: Run automated gates on a clean Windows checkout**

Run the commands in `docs/windows-release-runbook.md`: Rust format/Clippy/tests, Bun frozen install/test/build, Windows verification, installer build, relay verification, diagnostics audit, and Python script tests.

- [ ] **Step 2: Record real two-Windows acceptance**

Run `python scripts/create-windows-acceptance-record.py --operator "release-team"`, then bind pairing, approval, video, input, recovery, file/clipboard, sleep/wake, DPI/multi-display, and long-soak results to the exact installer SHA-256.

- [ ] **Step 3: Sign and publish only after readiness is true**

Configure the controlled signing identity, run `python scripts/build-windows-installer.py --require-signing`, run `python scripts/check-windows-release-ready.py --strict`, create annotated tag `v0.1.91`, and push the verified commit/tag. Without signing or two physical Windows records, keep the result as a candidate and do not create the formal release.

## Execution evidence (2026-07-31)

- Rust format, workspace tests and Clippy passed on macOS; Windows UI tests passed (`185` tests across `44` files) and the UI production build passed (`1806` modules).
- Python release script tests passed (`29` tests) under Python `3.9.6`; Cargo version parsing now uses `tomllib` when available and a bounded Python 3.9 fallback otherwise.
- macOS and iOS verification scripts passed; these remain build/permission gates and do not replace physical Apple-device acceptance.
- Managed relay bidirectional probe passed in `171 ms`; managed diagnostics audit passed.
- Commit `7ddf697` was pushed to `origin/main`. The candidate readiness report remains `ready: false` only for artifact provenance/signing/tag and real two-Windows acceptance gates; no formal `v0.1.91` tag was created.
- Windows CI run `30569603692` passed on commit `6d7d6d3`: Python policy tests, UI build, Rust fmt/Clippy/tests, Windows release verification, and unsigned candidate installer construction. The candidate installer SHA-256 is `4223032dfe2e806fd859b0eb6bd4dc22b56e1f237a04cf399aec55b31301ecbf`.
- The signed workflow now fails closed before publishing unless readiness is true and source-bound; the readiness report is published as evidence alongside the installer and verification manifests.
- Windows CI run `30571054699` passed on commit `f9d5925`; the candidate installer is `DeskLinkSetup-0.1.91-x64.exe` with SHA-256 `24446a222ae170e518f302a1bbde077d24353b7306719536460e23b2167ea27b`. Its readiness report is source-bound and correctly remains `ready: false` until signing, tag, fresh operations evidence, and physical Windows acceptance are supplied.
