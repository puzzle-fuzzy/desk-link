# iOS Mouse Modes and Edge Follow Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restore the iOS direct-touch and trackpad modes, make trackpad movement relative like ToDesk, keep the remote pointer visible by following it at viewport edges, and correct four-finger vertical pan direction.

**Architecture:** Keep the existing `IOSTouchMode` and `IOSTouchMapper` as the input boundary. Trackpad mode owns a persistent normalized remote pointer position and reports it to the session view; the session view asks `IOSVideoViewport` to pan only when that pointer approaches the visible edge. Direct mode continues mapping touch coordinates to the rendered video rect. The mode selector is a native menu attached to the existing lower-right control cluster so the video remains full screen.

**Tech Stack:** Swift 6, SwiftUI, UIKit touch events, Metal video surface, XCTest, iPhone simulator.

## Global Constraints

- Preserve the existing full-screen iOS session layout and fixed right-side controls.
- Keep direct touch and trackpad as the only two pointer modes.
- Trackpad touch-down must not teleport the remote pointer; movement must produce proportional relative movement.
- Four-finger drag must pan the zoomed video with corrected vertical direction.
- Preserve one-finger click/drag, long-press right click, two-finger pinch or wheel, and three-finger right click.
- Do not modify the user-owned Xcode project files: `apps/ios/DeskLinkIOS.xcodeproj/project.pbxproj`, `project.xcworkspace`, or `xcuserdata`.

---

### Task 1: Make trackpad mapping persistent and testable

**Files:**
- Modify: `apps/ios/DeskLinkIOS/Input/IOSTouchInputView.swift`
- Test: `apps/ios/DeskLinkIOSTests/IOSInputMapperTests.swift`

**Interfaces:**
- `IOSTouchMapper` retains `trackpadPosition` when the view receives video-rect updates.
- Add a trackpad pointer command helper for the current normalized pointer so three-finger right click works in both modes.
- Add a callback from `IOSTouchInputView` for the updated normalized trackpad position.

- [ ] **Step 1: Add failing mapper tests**

```swift
func testTrackpadMovementDoesNotUseTouchDownPosition() {
    var mapper = IOSTouchMapper(
        videoSize: CGSize(width: 1920, height: 1080),
        bounds: CGRect(x: 0, y: 0, width: 390, height: 844),
        mode: .trackpad
    )

    XCTAssertEqual(
        mapper.relativeCommand(delta: CGSize(width: 39, height: 0)),
        .move(normalizedX: 0.6, normalizedY: 0.5)
    )
}

func testTrackpadPointerCommandUsesPersistentPosition() {
    var mapper = IOSTouchMapper(
        videoSize: CGSize(width: 1920, height: 1080),
        bounds: CGRect(x: 0, y: 0, width: 390, height: 844),
        mode: .trackpad
    )

    _ = mapper.relativeCommand(delta: CGSize(width: 39, height: 0))
    XCTAssertEqual(mapper.currentPointerCommand, .move(normalizedX: 0.6, normalizedY: 0.5))
}
```

- [ ] **Step 2: Run the focused test and confirm it fails**

Run:

```bash
xcodebuild -quiet -project apps/ios/DeskLinkIOS.xcodeproj -scheme DeskLinkIOS -sdk iphonesimulator -destination 'id=6B14E42D-3A71-46D5-863E-1F4815E5DD36' -only-testing:DeskLinkIOSTests/IOSInputMapperTests test
```

Expected: the new persistent-pointer assertion is unavailable or fails before implementation.

- [ ] **Step 3: Preserve the mapper position and expose the current pointer command**

Add an initializer position parameter with a center default, preserve the old position in `TouchSurface.update` when the mode is unchanged, reset to center when switching modes, and add:

```swift
var currentPointerCommand: RemoteInputCommand? {
    guard mode == .trackpad else { return nil }
    return .move(
        normalizedX: Float(trackpadPosition.x),
        normalizedY: Float(trackpadPosition.y)
    )
}
```

Call the new position callback after each trackpad relative move and use `currentPointerCommand` for trackpad three-finger right click and long-press right click.

- [ ] **Step 4: Run the focused test and confirm it passes**

Expected: both new mapper tests pass without changing direct-touch mapping.

---

### Task 2: Add edge-following viewport behavior and correct four-finger pan

**Files:**
- Modify: `apps/ios/DeskLinkIOS/Input/IOSTouchInputView.swift`
- Modify: `apps/ios/DeskLinkIOS/Views/IOSSessionView.swift`
- Test: `apps/ios/DeskLinkIOSTests/IOSInputMapperTests.swift`

**Interfaces:**
- Add `IOSVideoViewport.keepPointerVisible(normalizedPosition:videoSize:bounds:edgeInset:)`.
- The trackpad callback calls `keepPointerVisible` with the persistent remote pointer position.
- `IOSTouchSurface` passes `IOSTouchMapper.fourFingerPanDelta(_:)` to the session callback, inverting only the vertical component before `IOSVideoViewport.pan` receives it.

- [ ] **Step 1: Add failing viewport tests**

```swift
func testViewportMovesToKeepTrackpadPointerInsideSafeEdge() {
    var viewport = IOSVideoViewport()
    viewport.pinch(
        factor: 2,
        anchor: CGPoint(x: 195, y: 422),
        videoSize: CGSize(width: 1920, height: 1080),
        bounds: CGSize(width: 390, height: 844)
    )

    viewport.keepPointerVisible(
        normalizedPosition: CGPoint(x: 1, y: 0.5),
        videoSize: CGSize(width: 1920, height: 1080),
        bounds: CGSize(width: 390, height: 844),
        edgeInset: 48
    )

    let baseRect = VideoGeometry.aspectFit(
        source: CGSize(width: 1920, height: 1080),
        in: CGRect(x: 0, y: 0, width: 390, height: 844)
    )
    XCTAssertLessThan(viewport.panOffset.width, 0)
    XCTAssertTrue(viewport.renderRect(baseRect: baseRect).intersects(CGRect(x: 0, y: 0, width: 390, height: 844)))
}
```

- [ ] **Step 2: Implement safe-edge correction and vertical inversion**

Compute the rendered remote pointer from the normalized position, compare it with `bounds.insetBy(dx: edgeInset, dy: edgeInset)`, add only the required pan correction, and reuse the existing pan clamp. Route four-finger deltas through `IOSTouchMapper.fourFingerPanDelta(_:)` before calling `viewport.pan`.

- [ ] **Step 3: Run the focused tests**

Expected: viewport zoom, safe-edge correction, direct mapping, trackpad mapping, and keyboard tests pass.

---

### Task 3: Restore the two-mode control in the full-screen session UI

**Files:**
- Modify: `apps/ios/DeskLinkIOS/Views/IOSSessionView.swift`

**Interfaces:**
- Add `@State private var touchMode: IOSTouchMode = .direct`.
- Pass `touchMode` and the trackpad-position callback into `IOSTouchInputView`.
- Add a native `Menu` control with “直接触控” and “轨迹板” options to the fixed lower-right controls.

- [ ] **Step 1: Add the mode state and menu**

Use the selected mode to choose `hand.tap` for direct touch and `cursorarrow` for trackpad, and expose the current mode through `accessibilityValue` and `session-mouse-mode`.

- [ ] **Step 2: Wire mode changes to touch lifecycle**

When the menu changes mode, `IOSTouchInputView.update` releases all active buttons and resets trackpad position to the center. The video viewport remains unchanged so switching modes does not unexpectedly zoom the user’s view.

- [ ] **Step 3: Run the iOS UI and unit tests**

Run:

```bash
xcodebuild -quiet -project apps/ios/DeskLinkIOS.xcodeproj -scheme DeskLinkIOS -sdk iphonesimulator -destination 'id=6B14E42D-3A71-46D5-863E-1F4815E5DD36' test
```

Expected: the complete iOS test suite passes; existing Xcode user files remain the only unrelated working-tree changes.

---

### Task 4: Review, commit, and push

**Files:**
- Review: `apps/ios/DeskLinkIOS/Input/IOSTouchInputView.swift`
- Review: `apps/ios/DeskLinkIOS/Views/IOSSessionView.swift`
- Review: `apps/ios/DeskLinkIOSTests/IOSInputMapperTests.swift`

- [ ] **Step 1: Run formatting and diff checks**

```bash
git diff --check
git status --short
```

Expected: no whitespace errors; only the intended iOS source/test files are staged.

- [ ] **Step 2: Commit the implementation**

```bash
git add apps/ios/DeskLinkIOS/Input/IOSTouchInputView.swift apps/ios/DeskLinkIOS/Views/IOSSessionView.swift apps/ios/DeskLinkIOSTests/IOSInputMapperTests.swift
git commit -m "fix(ios): restore trackpad mode and edge-following pointer"
```

- [ ] **Step 3: Push and verify**

```bash
git push origin main
git status --short
```

Expected: `origin/main` contains the new commit and only the pre-existing Xcode project user files remain unstaged.
