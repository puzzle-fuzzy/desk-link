import DeskLinkAppleCore
import SwiftUI
import UIKit

enum IOSTouchMode: String, CaseIterable, Identifiable {
    case direct = "直接触控"
    case trackpad = "轨迹板"

    var id: Self { self }
}

enum IOSTouchPhase {
    case began
    case moved
    case ended
    case cancelled
}

struct IOSTouchMapper {
    static let maxWheelDelta: Int32 = 1_200

    let videoSize: CGSize
    let bounds: CGRect
    let mode: IOSTouchMode
    let visibleVideoRect: CGRect
    private(set) var trackpadPosition = CGPoint(x: 0.5, y: 0.5)

    init(videoSize: CGSize, bounds: CGRect, mode: IOSTouchMode, visibleVideoRect: CGRect? = nil) {
        self.videoSize = videoSize
        self.bounds = bounds
        self.mode = mode
        self.visibleVideoRect = visibleVideoRect ?? VideoGeometry.aspectFit(source: videoSize, in: bounds)
    }

    func command(for point: CGPoint, phase: IOSTouchPhase) -> RemoteInputCommand? {
        guard mode == .direct,
              visibleVideoRect.width > 0,
              visibleVideoRect.height > 0,
              visibleVideoRect.contains(point)
        else { return nil }

        let x = Float((point.x - visibleVideoRect.minX) / visibleVideoRect.width)
        let y = Float((point.y - visibleVideoRect.minY) / visibleVideoRect.height)
        return .move(normalizedX: x.clamped(to: 0...1), normalizedY: y.clamped(to: 0...1))
    }

    mutating func relativeCommand(delta: CGSize) -> RemoteInputCommand? {
        guard mode == .trackpad, bounds.width > 0, bounds.height > 0 else { return nil }
        trackpadPosition.x = (trackpadPosition.x + delta.width / bounds.width).clamped(to: 0...1)
        trackpadPosition.y = (trackpadPosition.y + delta.height / bounds.height).clamped(to: 0...1)
        return .move(
            normalizedX: Float(trackpadPosition.x),
            normalizedY: Float(trackpadPosition.y)
        )
    }

    static func wheel(deltaX: Int32, deltaY: Int32) -> RemoteInputCommand {
        .wheel(
            deltaX: deltaX.clamped(to: -maxWheelDelta...maxWheelDelta),
            deltaY: deltaY.clamped(to: -maxWheelDelta...maxWheelDelta)
        )
    }
}

struct IOSTouchInputView: UIViewRepresentable {
    let bridge: ControllerBridge
    let videoSize: CGSize?
    let visibleVideoRect: CGRect
    let mode: IOSTouchMode

    func makeUIView(context: Context) -> TouchSurface {
        TouchSurface(
            bridge: bridge,
            videoSize: videoSize,
            visibleVideoRect: visibleVideoRect,
            mode: mode
        )
    }

    func updateUIView(_ view: TouchSurface, context: Context) {
        view.update(videoSize: videoSize, visibleVideoRect: visibleVideoRect, mode: mode)
    }

    static func dismantleUIView(_ view: TouchSurface, coordinator: ()) {
        view.releaseAll()
    }

    final class TouchSurface: UIView {
        weak var bridge: ControllerBridge?
        private var videoSize: CGSize?
        private var visibleVideoRect: CGRect
        private var mode: IOSTouchMode
        private var touchMapper: IOSTouchMapper
        private var trackedTouches: [ObjectIdentifier: UITouch] = [:]
        private var primaryTouchID: ObjectIdentifier?
        private var startPoint: CGPoint = .zero
        private var lastCenter: CGPoint = .zero
        private var moved = false
        private var isTwoFingerGesture = false
        private var longPressTriggered = false
        private var activeButton: MouseButton?
        private var longPressWorkItem: DispatchWorkItem?

        init(bridge: ControllerBridge, videoSize: CGSize?, visibleVideoRect: CGRect, mode: IOSTouchMode) {
            self.bridge = bridge
            self.videoSize = videoSize
            self.visibleVideoRect = visibleVideoRect
            self.mode = mode
            self.touchMapper = IOSTouchMapper(
                videoSize: videoSize ?? .zero,
                bounds: .zero,
                mode: mode,
                visibleVideoRect: visibleVideoRect.isEmpty ? nil : visibleVideoRect
            )
            super.init(frame: .zero)
            isMultipleTouchEnabled = true
            backgroundColor = .clear
        }

        required init?(coder: NSCoder) { nil }

        func update(videoSize: CGSize?, visibleVideoRect: CGRect, mode: IOSTouchMode) {
            if self.mode != mode {
                releaseAll()
            }
            self.videoSize = videoSize
            self.visibleVideoRect = visibleVideoRect
            self.mode = mode
            touchMapper = makeMapper()
        }

        override func layoutSubviews() {
            super.layoutSubviews()
            touchMapper = makeMapper()
        }

        override func didMoveToWindow() {
            super.didMoveToWindow()
            if window == nil { releaseAll() }
        }

        override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
            for touch in touches { trackedTouches[ObjectIdentifier(touch)] = touch }
            guard primaryTouchID == nil, let primary = touches.first else {
                if trackedTouches.count >= 2 {
                    isTwoFingerGesture = true
                    cancelLongPress()
                    lastCenter = centerOfTrackedTouches()
                }
                return
            }
            primaryTouchID = ObjectIdentifier(primary)
            startPoint = primary.location(in: self)
            lastCenter = startPoint
            moved = false
            isTwoFingerGesture = trackedTouches.count >= 2
            longPressTriggered = false
            activeButton = nil
            if !isTwoFingerGesture { scheduleLongPress() }
        }

        override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
            for touch in touches { trackedTouches[ObjectIdentifier(touch)] = touch }
            guard primaryTouchID != nil else { return }
            if trackedTouches.count >= 2 { isTwoFingerGesture = true }
            let center = centerOfTrackedTouches()
            let delta = CGSize(width: center.x - lastCenter.x, height: center.y - lastCenter.y)
            lastCenter = center
            if abs(center.x - startPoint.x) > 8 || abs(center.y - startPoint.y) > 8 {
                moved = true
                cancelLongPress()
            }

            if isTwoFingerGesture {
                guard mode == .trackpad, delta.width != 0 || delta.height != 0 else { return }
                bridge?.send(input: IOSTouchMapper.wheel(
                    deltaX: Int32((delta.width * 8).rounded()),
                    deltaY: Int32((-delta.height * 8).rounded())
                ))
                return
            }

            if mode == .direct {
                guard let primaryTouchID,
                      let primary = trackedTouches[primaryTouchID]
                else { return }
                let point = primary.location(in: self)
                guard let command = touchMapper.command(for: point, phase: .moved) else { return }
                if activeButton == nil, moved {
                    bridge?.send(input: command)
                    bridge?.send(input: .mouseButton(.left, pressed: true))
                    activeButton = .left
                } else if activeButton != nil {
                    bridge?.send(input: command)
                }
            } else if let command = touchMapper.relativeCommand(delta: delta), moved {
                bridge?.send(input: command)
            }
        }

        override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
            let endedPrimary = touches.contains { ObjectIdentifier($0) == primaryTouchID }
            for touch in touches { trackedTouches.removeValue(forKey: ObjectIdentifier(touch)) }
            guard endedPrimary else { return }
            cancelLongPress()

            if let activeButton {
                bridge?.send(input: .mouseButton(activeButton, pressed: false))
            } else if longPressTriggered {
                bridge?.send(input: .mouseButton(.right, pressed: false))
            } else if !moved, !isTwoFingerGesture, let primary = touches.first {
                let point = primary.location(in: self)
                if mode == .direct, let command = touchMapper.command(for: point, phase: .ended) {
                    bridge?.send(input: command)
                    bridge?.send(input: .mouseButton(.left, pressed: true))
                    bridge?.send(input: .mouseButton(.left, pressed: false))
                } else if mode == .trackpad {
                    bridge?.send(input: .mouseButton(.left, pressed: true))
                    bridge?.send(input: .mouseButton(.left, pressed: false))
                }
            }
            resetGestureState()
        }

        override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
            releaseAll()
        }

        func releaseAll() {
            cancelLongPress()
            bridge?.releaseAll()
            resetGestureState()
        }

        private func makeMapper() -> IOSTouchMapper {
            IOSTouchMapper(
                videoSize: videoSize ?? .zero,
                bounds: bounds,
                mode: mode,
                visibleVideoRect: visibleVideoRect.isEmpty ? nil : visibleVideoRect
            )
        }

        private func scheduleLongPress() {
            let work = DispatchWorkItem { [weak self] in self?.triggerLongPress() }
            longPressWorkItem = work
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.45, execute: work)
        }

        private func triggerLongPress() {
            guard !moved, !isTwoFingerGesture, !longPressTriggered else { return }
            longPressTriggered = true
            if let primaryTouchID,
               let primary = trackedTouches[primaryTouchID],
               touchMapper.command(for: primary.location(in: self), phase: .began) != nil
            {
                bridge?.send(input: .mouseButton(.right, pressed: true))
            }
        }

        private func cancelLongPress() {
            longPressWorkItem?.cancel()
            longPressWorkItem = nil
        }

        private func resetGestureState() {
            trackedTouches.removeAll()
            primaryTouchID = nil
            activeButton = nil
            moved = false
            isTwoFingerGesture = false
            longPressTriggered = false
        }

        private func centerOfTrackedTouches() -> CGPoint {
            guard !trackedTouches.isEmpty else { return lastCenter }
            let points = trackedTouches.values.map { $0.location(in: self) }
            return CGPoint(
                x: points.map(\.x).reduce(0, +) / CGFloat(points.count),
                y: points.map(\.y).reduce(0, +) / CGFloat(points.count)
            )
        }
    }
}

private extension Comparable {
    func clamped(to range: ClosedRange<Self>) -> Self {
        min(max(self, range.lowerBound), range.upperBound)
    }
}
