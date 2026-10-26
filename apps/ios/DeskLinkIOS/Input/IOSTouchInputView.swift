import DeskLinkAppleCore
import SwiftUI
import UIKit

enum IOSTouchMode: String, CaseIterable, Hashable, Identifiable {
    case direct = "直接触控"
    case trackpad = "轨迹板"

    var id: Self { self }

    var systemImage: String {
        switch self {
        case .direct: "hand.tap"
        case .trackpad: "cursorarrow"
        }
    }
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
    private(set) var trackpadPosition: CGPoint

    init(
        videoSize: CGSize,
        bounds: CGRect,
        mode: IOSTouchMode,
        visibleVideoRect: CGRect? = nil,
        trackpadPosition: CGPoint = CGPoint(x: 0.5, y: 0.5)
    ) {
        self.videoSize = videoSize
        self.bounds = bounds
        self.mode = mode
        self.visibleVideoRect = visibleVideoRect ?? VideoGeometry.aspectFit(source: videoSize, in: bounds)
        self.trackpadPosition = CGPoint(
            x: trackpadPosition.x.clamped(to: 0...1),
            y: trackpadPosition.y.clamped(to: 0...1)
        )
    }

    func command(for point: CGPoint, phase: IOSTouchPhase) -> RemoteInputCommand? {
        guard mode == .direct,
              let normalized = VideoGeometry.normalizedTopLeftPoint(
                  for: point,
                  in: visibleVideoRect
              )
        else { return nil }

        return .move(
            normalizedX: Float(normalized.x).clamped(to: 0...1),
            normalizedY: Float(normalized.y).clamped(to: 0...1)
        )
    }

    mutating func relativeCommand(delta: CGSize) -> RemoteInputCommand? {
        guard mode == .trackpad,
              videoSize.width > 0,
              videoSize.height > 0
        else { return nil }

        // Trackpad mode is a relative, 1:1 input. The command carries a
        // normalized position, so use the remote video pixels as its scale
        // instead of the iPhone view size (which would amplify the motion).
        trackpadPosition.x = (trackpadPosition.x + delta.width / videoSize.width).clamped(to: 0...1)
        trackpadPosition.y = (trackpadPosition.y + delta.height / videoSize.height).clamped(to: 0...1)
        return currentPointerCommand
    }

    var currentPointerCommand: RemoteInputCommand? {
        guard mode == .trackpad else { return nil }
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

    static func fourFingerPanDelta(_ delta: CGSize) -> CGSize {
        CGSize(width: delta.width, height: -delta.height)
    }
}

struct IOSVideoViewport: Equatable {
    private(set) var zoomScale: CGFloat = 1
    private(set) var panOffset: CGSize = .zero

    mutating func pinch(
        factor: CGFloat,
        anchor: CGPoint,
        videoSize: CGSize?,
        bounds: CGSize
    ) {
        guard factor.isFinite, factor > 0,
              let videoSize,
              videoSize.width > 0,
              videoSize.height > 0,
              bounds.width > 0,
              bounds.height > 0
        else { return }

        let baseRect = VideoGeometry.aspectFit(
            source: videoSize,
            in: CGRect(origin: .zero, size: bounds)
        )
        guard !baseRect.isEmpty else { return }

        let oldRect = renderRect(baseRect: baseRect)
        let nextScale = (zoomScale * factor).clamped(to: 1...4)
        let appliedFactor = nextScale / zoomScale
        let nextOrigin = CGPoint(
            x: anchor.x - (anchor.x - oldRect.minX) * appliedFactor,
            y: anchor.y - (anchor.y - oldRect.minY) * appliedFactor
        )
        let nextSize = CGSize(
            width: baseRect.width * nextScale,
            height: baseRect.height * nextScale
        )
        let centeredOrigin = CGPoint(
            x: baseRect.midX - nextSize.width / 2,
            y: baseRect.midY - nextSize.height / 2
        )

        zoomScale = nextScale
        panOffset = CGSize(
            width: nextOrigin.x - centeredOrigin.x,
            height: nextOrigin.y - centeredOrigin.y
        )
        clampPan(baseRect: baseRect)
    }

    mutating func pan(delta: CGSize, videoSize: CGSize?, bounds: CGSize) {
        guard zoomScale > 1,
              let videoSize,
              videoSize.width > 0,
              videoSize.height > 0,
              bounds.width > 0,
              bounds.height > 0
        else { return }

        let baseRect = VideoGeometry.aspectFit(
            source: videoSize,
            in: CGRect(origin: .zero, size: bounds)
        )
        panOffset.width += delta.width
        panOffset.height += delta.height
        clampPan(baseRect: baseRect)
    }

    func renderRect(baseRect: CGRect) -> CGRect {
        iosRenderedVideoRect(
            baseRect: baseRect,
            zoomScale: zoomScale,
            panOffset: panOffset
        )
    }

    mutating func reset() {
        zoomScale = 1
        panOffset = .zero
    }

    mutating func keepPointerVisible(
        normalizedPosition: CGPoint,
        videoSize: CGSize?,
        bounds: CGSize,
        edgeInset: CGFloat = 64
    ) {
        guard let videoSize,
              videoSize.width > 0,
              videoSize.height > 0,
              bounds.width > 0,
              bounds.height > 0
        else { return }

        let baseRect = VideoGeometry.aspectFit(
            source: videoSize,
            in: CGRect(origin: .zero, size: bounds)
        )
        guard !baseRect.isEmpty else { return }

        let normalized = CGPoint(
            x: normalizedPosition.x.clamped(to: 0...1),
            y: normalizedPosition.y.clamped(to: 0...1)
        )
        let renderedRect = renderRect(baseRect: baseRect)
        let pointer = CGPoint(
            x: renderedRect.minX + renderedRect.width * normalized.x,
            y: renderedRect.minY + renderedRect.height * normalized.y
        )
        let inset = min(max(edgeInset, 0), min(bounds.width, bounds.height) / 2)
        let safeRect = CGRect(origin: .zero, size: bounds).insetBy(dx: inset, dy: inset)
        var correction = CGSize.zero
        if pointer.x < safeRect.minX {
            correction.width = safeRect.minX - pointer.x
        } else if pointer.x > safeRect.maxX {
            correction.width = safeRect.maxX - pointer.x
        }
        if pointer.y < safeRect.minY {
            correction.height = safeRect.minY - pointer.y
        } else if pointer.y > safeRect.maxY {
            correction.height = safeRect.maxY - pointer.y
        }

        panOffset.width += correction.width
        panOffset.height += correction.height
        clampPan(baseRect: baseRect)
    }

    private mutating func clampPan(baseRect: CGRect) {
        guard zoomScale > 1 else {
            panOffset = .zero
            return
        }

        let maxX = max((baseRect.width * zoomScale - baseRect.width) / 2, 0)
        let maxY = max((baseRect.height * zoomScale - baseRect.height) / 2, 0)
        panOffset.width = panOffset.width.clamped(to: -maxX...maxX)
        panOffset.height = panOffset.height.clamped(to: -maxY...maxY)
    }
}

func iosRenderedVideoRect(
    baseRect: CGRect,
    zoomScale: CGFloat,
    panOffset: CGSize
) -> CGRect {
    let size = CGSize(
        width: baseRect.width * zoomScale,
        height: baseRect.height * zoomScale
    )
    let centeredOrigin = CGPoint(
        x: baseRect.midX - size.width / 2,
        y: baseRect.midY - size.height / 2
    )
    return CGRect(
        origin: CGPoint(
            x: centeredOrigin.x + panOffset.width,
            y: centeredOrigin.y + panOffset.height
        ),
        size: size
    )
}

private final class IOSInputFocusObservation: @unchecked Sendable {
    var token: NSObjectProtocol?
    deinit { if let token { NotificationCenter.default.removeObserver(token) } }
}

struct IOSGestureInputCapture: Sendable {
    struct Ticket: Equatable, Sendable {
        let context: CapturedInputContext
        let serial: UInt64
    }
    private(set) var ticket: Ticket?
    private var serial: UInt64 = 0

    mutating func begin(context: CapturedInputContext) {
        serial &+= 1
        ticket = Ticket(context: context, serial: serial)
    }

    func accepts(_ candidate: Ticket) -> Bool { ticket == candidate }

    func matchesInputTarget(_ context: CapturedInputContext?) -> Bool {
        guard let context, let ticket else { return false }
        return ticket.context.hasSameInputTarget(as: context)
    }

    mutating func cancel() {
        serial &+= 1
        ticket = nil
    }
}

struct IOSTouchInputView: UIViewRepresentable {
    let bridge: ControllerBridge
    let videoSize: CGSize?
    let visibleVideoRect: CGRect
    let mode: IOSTouchMode
    let onPinchChanged: (CGFloat, CGPoint) -> Void
    let onFourFingerPan: (CGSize) -> Void
    let onTrackpadPositionChanged: (CGPoint) -> Void

    init(
        bridge: ControllerBridge,
        videoSize: CGSize?,
        visibleVideoRect: CGRect,
        mode: IOSTouchMode,
        onPinchChanged: @escaping (CGFloat, CGPoint) -> Void = { _, _ in },
        onFourFingerPan: @escaping (CGSize) -> Void = { _ in },
        onTrackpadPositionChanged: @escaping (CGPoint) -> Void = { _ in }
    ) {
        self.bridge = bridge
        self.videoSize = videoSize
        self.visibleVideoRect = visibleVideoRect
        self.mode = mode
        self.onPinchChanged = onPinchChanged
        self.onFourFingerPan = onFourFingerPan
        self.onTrackpadPositionChanged = onTrackpadPositionChanged
    }

    func makeUIView(context: Context) -> TouchSurface {
        TouchSurface(
            bridge: bridge,
            videoSize: videoSize,
            visibleVideoRect: visibleVideoRect,
            mode: mode,
            onPinchChanged: onPinchChanged,
            onFourFingerPan: onFourFingerPan,
            onTrackpadPositionChanged: onTrackpadPositionChanged
        )
    }

    func updateUIView(_ view: TouchSurface, context: Context) {
        view.update(
            videoSize: videoSize,
            visibleVideoRect: visibleVideoRect,
            mode: mode,
            onPinchChanged: onPinchChanged,
            onFourFingerPan: onFourFingerPan,
            onTrackpadPositionChanged: onTrackpadPositionChanged
        )
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
        private var multiTouchGesture: MultiTouchGesture = .none
        private var multiTouchFingerCount = 0
        private var lastPinchDistance: CGFloat = 0
        private var longPressTriggered = false
        private var activeButton: MouseButton?
        private var longPressWorkItem: DispatchWorkItem?
        private var gestureCapture = IOSGestureInputCapture()
        private var surfaceContext: CapturedInputContext?
        private let focusObservation = IOSInputFocusObservation()
        private var onPinchChanged: (CGFloat, CGPoint) -> Void
        private var onFourFingerPan: (CGSize) -> Void
        private var onTrackpadPositionChanged: (CGPoint) -> Void

        private enum MultiTouchGesture: Equatable {
            case none
            case twoFinger
            case threeFingerTap
            case fourFingerPan
        }

        init(
            bridge: ControllerBridge,
            videoSize: CGSize?,
            visibleVideoRect: CGRect,
            mode: IOSTouchMode,
            onPinchChanged: @escaping (CGFloat, CGPoint) -> Void,
            onFourFingerPan: @escaping (CGSize) -> Void,
            onTrackpadPositionChanged: @escaping (CGPoint) -> Void
        ) {
            self.bridge = bridge
            self.surfaceContext = bridge.presentedInputContext
            self.videoSize = videoSize
            self.visibleVideoRect = visibleVideoRect
            self.mode = mode
            self.onPinchChanged = onPinchChanged
            self.onFourFingerPan = onFourFingerPan
            self.onTrackpadPositionChanged = onTrackpadPositionChanged
            self.touchMapper = IOSTouchMapper(
                videoSize: videoSize ?? .zero,
                bounds: .zero,
                mode: mode,
                visibleVideoRect: visibleVideoRect.isEmpty ? nil : visibleVideoRect
            )
            super.init(frame: .zero)
            isMultipleTouchEnabled = true
            backgroundColor = .clear
            focusObservation.token = NotificationCenter.default.addObserver(
                forName: UIApplication.willResignActiveNotification, object: nil, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.releaseAll() }
            }
        }

        required init?(coder: NSCoder) { nil }

        func update(
            videoSize: CGSize?,
            visibleVideoRect: CGRect,
            mode: IOSTouchMode,
            onPinchChanged: @escaping (CGFloat, CGPoint) -> Void,
            onFourFingerPan: @escaping (CGSize) -> Void,
            onTrackpadPositionChanged: @escaping (CGPoint) -> Void
        ) {
            synchronizeInputContext(bridge?.presentedInputContext)
            let modeChanged = self.mode != mode
            if modeChanged {
                releaseAll()
            }
            self.videoSize = videoSize
            self.visibleVideoRect = visibleVideoRect
            self.mode = mode
            self.onPinchChanged = onPinchChanged
            self.onFourFingerPan = onFourFingerPan
            self.onTrackpadPositionChanged = onTrackpadPositionChanged
            touchMapper = makeMapper(
                trackpadPosition: modeChanged ? CGPoint(x: 0.5, y: 0.5) : touchMapper.trackpadPosition
            )
        }

        override func layoutSubviews() {
            super.layoutSubviews()
            touchMapper = makeMapper(trackpadPosition: touchMapper.trackpadPosition)
        }

        override func didMoveToWindow() {
            super.didMoveToWindow()
            if window == nil { releaseAll() }
        }

        override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
            if trackedTouches.isEmpty {
                guard let context = bridge?.captureInputContext() else { return }
                synchronizeInputContext(context)
                gestureCapture.begin(context: context)
            }
            guard gestureCapture.ticket != nil else { return }
            for touch in touches { trackedTouches[ObjectIdentifier(touch)] = touch }
            if trackedTouches.count >= 2 {
                if multiTouchGesture == .none || trackedTouches.count > multiTouchFingerCount {
                    beginMultiTouchGesture()
                }
                return
            }
            guard primaryTouchID == nil, let primary = touches.first else { return }
            primaryTouchID = ObjectIdentifier(primary)
            startPoint = primary.location(in: self)
            lastCenter = startPoint
            moved = false
            longPressTriggered = false
            activeButton = nil
            if mode == .direct, let command = touchMapper.command(for: startPoint, phase: .began) {
                sendGesture(command)
            }
            scheduleLongPress()
        }

        override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
            guard gestureCapture.ticket != nil else { return }
            guard gestureCapture.matchesInputTarget(bridge?.captureInputContext()) else { releaseAll(); return }
            let knownTouches = touches.filter { trackedTouches[ObjectIdentifier($0)] != nil }
            guard !knownTouches.isEmpty else { return }
            for touch in knownTouches { trackedTouches[ObjectIdentifier(touch)] = touch }
            if trackedTouches.count >= 2 {
                if multiTouchGesture == .none || trackedTouches.count > multiTouchFingerCount {
                    beginMultiTouchGesture()
                }
                let center = centerOfTrackedTouches()
                let delta = CGSize(width: center.x - lastCenter.x, height: center.y - lastCenter.y)
                lastCenter = center
                if abs(delta.width) > 1 || abs(delta.height) > 1 { moved = true }

                switch multiTouchGesture {
                case .twoFinger:
                    if mode == .direct {
                        let distance = distanceBetweenTrackedTouches()
                        if distance > 0, lastPinchDistance > 0 {
                            onPinchChanged(distance / lastPinchDistance, center)
                        }
                        if distance > 0 { lastPinchDistance = distance }
                    } else if delta.width != 0 || delta.height != 0 {
                        sendGesture(IOSTouchMapper.wheel(
                            deltaX: Int32((delta.width * 8).rounded()),
                            deltaY: Int32((-delta.height * 8).rounded())
                        ))
                    }
                case .fourFingerPan:
                    onFourFingerPan(IOSTouchMapper.fourFingerPanDelta(delta))
                case .threeFingerTap, .none:
                    break
                }
                return
            }
            guard primaryTouchID != nil else { return }
            let center = centerOfTrackedTouches()
            let delta = CGSize(width: center.x - lastCenter.x, height: center.y - lastCenter.y)
            lastCenter = center
            if abs(center.x - startPoint.x) > 8 || abs(center.y - startPoint.y) > 8 {
                moved = true
                cancelLongPress()
            }

            if mode == .direct {
                guard let primaryTouchID,
                      let primary = trackedTouches[primaryTouchID]
                else { return }
                let point = primary.location(in: self)
                guard let command = touchMapper.command(for: point, phase: .moved) else { return }
                if activeButton == nil, moved {
                    sendGesture(command)
                    sendGesture(.mouseButton(.left, pressed: true))
                    activeButton = .left
                } else if activeButton != nil {
                    sendGesture(command)
                }
            } else if moved, let command = touchMapper.relativeCommand(delta: delta) {
                sendGesture(command)
                onTrackpadPositionChanged(touchMapper.trackpadPosition)
            }
        }

        override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
            guard gestureCapture.ticket != nil else { return }
            let endedPrimary = touches.contains { ObjectIdentifier($0) == primaryTouchID }
            let endingCenter = centerOfTrackedTouches()
            for touch in touches { trackedTouches.removeValue(forKey: ObjectIdentifier(touch)) }
            if !trackedTouches.isEmpty { return }
            cancelLongPress()

            if multiTouchGesture != .none {
                if multiTouchGesture == .threeFingerTap, !moved,
                   let command = pointerCommand(at: endingCenter)
                {
                    sendGesture(command)
                    sendGesture(.mouseButton(.right, pressed: true))
                    sendGesture(.mouseButton(.right, pressed: false))
                }
                resetGestureState()
                return
            }

            guard endedPrimary else { return }

            if let activeButton {
                sendGesture(.mouseButton(activeButton, pressed: false))
            } else if longPressTriggered {
                sendGesture(.mouseButton(.right, pressed: false))
            } else if !moved, let primary = touches.first {
                let point = primary.location(in: self)
                if mode == .direct, let command = touchMapper.command(for: point, phase: .ended) {
                    sendGesture(command)
                    sendGesture(.mouseButton(.left, pressed: true))
                    sendGesture(.mouseButton(.left, pressed: false))
                } else if mode == .trackpad {
                    sendGesture(.mouseButton(.left, pressed: true))
                    sendGesture(.mouseButton(.left, pressed: false))
                }
            }
            resetGestureState()
        }

        override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
            guard touches.contains(where: { trackedTouches[ObjectIdentifier($0)] != nil }) else { return }
            releaseAll()
        }

        func releaseAll() {
            cancelLongPress()
            if let context = gestureCapture.ticket?.context { bridge?.releaseAll(context: context) }
            resetGestureState()
        }

        func synchronizeInputContext(_ context: CapturedInputContext?) {
            guard context?.targetIdentity != surfaceContext?.targetIdentity else { return }
            releaseAll()
            surfaceContext = context
        }

        private func sendGesture(_ command: RemoteInputCommand) {
            guard let context = gestureCapture.ticket?.context else { return }
            bridge?.send(input: command, context: context)
        }

        private func makeMapper(trackpadPosition: CGPoint? = nil) -> IOSTouchMapper {
            IOSTouchMapper(
                videoSize: videoSize ?? .zero,
                bounds: bounds,
                mode: mode,
                visibleVideoRect: visibleVideoRect.isEmpty ? nil : visibleVideoRect,
                trackpadPosition: trackpadPosition ?? self.touchMapper.trackpadPosition
            )
        }

        private func pointerCommand(at point: CGPoint) -> RemoteInputCommand? {
            if mode == .direct {
                return touchMapper.command(for: point, phase: .ended)
            }
            return touchMapper.currentPointerCommand
        }

        private func scheduleLongPress() {
            guard let ticket = gestureCapture.ticket else { return }
            let work = DispatchWorkItem { [weak self, ticket] in self?.triggerLongPress(ticket: ticket) }
            longPressWorkItem = work
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.45, execute: work)
        }

        private func triggerLongPress(ticket: IOSGestureInputCapture.Ticket) {
            guard gestureCapture.accepts(ticket), !moved, multiTouchGesture == .none, !longPressTriggered else { return }
            longPressTriggered = true
            if let primaryTouchID,
               let primary = trackedTouches[primaryTouchID],
               pointerCommand(at: primary.location(in: self)) != nil
            {
                bridge?.send(input: .mouseButton(.right, pressed: true), context: ticket.context)
            }
        }

        private func cancelLongPress() {
            longPressWorkItem?.cancel()
            longPressWorkItem = nil
        }

        private func resetGestureState() {
            gestureCapture.cancel()
            trackedTouches.removeAll()
            primaryTouchID = nil
            activeButton = nil
            moved = false
            multiTouchGesture = .none
            multiTouchFingerCount = 0
            lastPinchDistance = 0
            longPressTriggered = false
        }

        private func beginMultiTouchGesture() {
            cancelLongPress()
            if let context = gestureCapture.ticket?.context { bridge?.releaseAll(context: context) }
            primaryTouchID = nil
            activeButton = nil
            multiTouchFingerCount = min(trackedTouches.count, 4)
            multiTouchGesture = switch multiTouchFingerCount {
            case 2: .twoFinger
            case 3: .threeFingerTap
            default: .fourFingerPan
            }
            lastCenter = centerOfTrackedTouches()
            lastPinchDistance = distanceBetweenTrackedTouches()
        }

        private func centerOfTrackedTouches() -> CGPoint {
            guard !trackedTouches.isEmpty else { return lastCenter }
            let points = trackedTouches.values.map { $0.location(in: self) }
            return CGPoint(
                x: points.map(\.x).reduce(0, +) / CGFloat(points.count),
                y: points.map(\.y).reduce(0, +) / CGFloat(points.count)
            )
        }

        private func distanceBetweenTrackedTouches() -> CGFloat {
            guard trackedTouches.count >= 2 else { return 0 }
            let points = trackedTouches.values.prefix(2).map { $0.location(in: self) }
            guard points.count == 2 else { return 0 }
            let dx = points[0].x - points[1].x
            let dy = points[0].y - points[1].y
            return sqrt(dx * dx + dy * dy)
        }
    }
}

private extension Comparable {
    func clamped(to range: ClosedRange<Self>) -> Self {
        min(max(self, range.lowerBound), range.upperBound)
    }
}
