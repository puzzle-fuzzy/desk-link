import CoreGraphics
import CoreMedia
import CoreVideo
import Foundation
import ScreenCaptureKit

private final class CapturedPixelBuffer: @unchecked Sendable {
    let value: CVPixelBuffer

    init(_ value: CVPixelBuffer) {
        self.value = value
    }
}

private final class LatestFrameDelivery: @unchecked Sendable {
    typealias Handler = @Sendable (CVPixelBuffer, UInt64) -> Void

    private let queue = DispatchQueue(label: "com.desklink.capture.delivery", qos: .userInitiated)
    private let lock = NSLock()
    private var streamIdentifier: ObjectIdentifier?
    private var nextFrameID: UInt64 = 0
    private var pending: (CapturedPixelBuffer, UInt64)?
    private var draining = false
    private var handler: Handler?

    func begin(stream: SCStream, handler: @escaping Handler) {
        lock.lock()
        streamIdentifier = ObjectIdentifier(stream)
        nextFrameID = 0
        pending = nil
        self.handler = handler
        lock.unlock()
    }

    func end(stream: SCStream? = nil) {
        lock.lock()
        let matchesStream = stream.map { streamIdentifier == ObjectIdentifier($0) } ?? true
        if matchesStream {
            streamIdentifier = nil
            nextFrameID = 0
            pending = nil
            handler = nil
        }
        lock.unlock()
    }

    func submit(stream: SCStream, buffer: CVPixelBuffer) {
        let shouldSchedule: Bool
        lock.lock()
        guard streamIdentifier == ObjectIdentifier(stream), handler != nil else {
            lock.unlock()
            return
        }
        nextFrameID &+= 1
        pending = (CapturedPixelBuffer(buffer), nextFrameID)
        shouldSchedule = !draining
        draining = true
        lock.unlock()
        if shouldSchedule {
            queue.async { [weak self] in self?.drain() }
        }
    }

    private func drain() {
        while true {
            lock.lock()
            guard let pending, let handler else {
                draining = false
                lock.unlock()
                return
            }
            self.pending = nil
            lock.unlock()
            handler(pending.0.value, pending.1)
        }
    }
}

@MainActor
final class ScreenCaptureSource: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    typealias FrameHandler = @Sendable (CVPixelBuffer, UInt64) -> Void
    typealias StopHandler = @Sendable (Error) -> Void

    private let sampleQueue = DispatchQueue(label: "com.desklink.capture.sample", qos: .userInteractive)
    nonisolated private let delivery = LatestFrameDelivery()
    private var stream: SCStream?
    private var selectedStreamID: UInt64 = 0
    private var stopHandler: StopHandler?

    private(set) var capturedDisplayFrame: CGRect = .zero

    func start(
        displayID: CGDirectDisplayID,
        streamID: UInt64,
        configuration: SCStreamConfiguration = SCStreamConfiguration(),
        onFrame: @escaping FrameHandler,
        onStop: @escaping StopHandler = { _ in }
    ) async throws {
        await stop()
        try Task.checkCancellation()
        let content = try await SCShareableContent.excludingDesktopWindows(
            false,
            onScreenWindowsOnly: true
        )
        try Task.checkCancellation()
        let mainMenuBarOrigin = CGDisplayBounds(CGMainDisplayID()).origin
        let display = content.displays.first(where: { $0.displayID == displayID })
            ?? content.displays.first(where: { $0.frame.contains(mainMenuBarOrigin) })
            ?? content.displays.first
        guard let display else { throw ScreenCaptureSourceError.displayUnavailable }

        configuration.width = display.width
        configuration.height = display.height
        configuration.pixelFormat = kCVPixelFormatType_32BGRA
        configuration.queueDepth = 3
        configuration.minimumFrameInterval = CMTime(value: 1, timescale: 30)

        let stream = SCStream(
            filter: SCContentFilter(display: display, excludingWindows: []),
            configuration: configuration,
            delegate: self
        )
        try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: sampleQueue)
        try Task.checkCancellation()
        self.stream = stream
        selectedStreamID = streamID
        capturedDisplayFrame = display.frame
        stopHandler = onStop
        delivery.begin(stream: stream, handler: onFrame)
        do {
            try await stream.startCapture()
        } catch {
            delivery.end(stream: stream)
            self.stream = nil
            selectedStreamID = 0
            capturedDisplayFrame = .zero
            stopHandler = nil
            throw error
        }
    }

    func stop() async {
        let stream = self.stream
        self.stream = nil
        selectedStreamID = 0
        capturedDisplayFrame = .zero
        stopHandler = nil
        delivery.end(stream: stream)
        if let stream { try? await stream.stopCapture() }
    }

    nonisolated func stream(
        _ stream: SCStream,
        didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of outputType: SCStreamOutputType
    ) {
        guard outputType == .screen,
              CMSampleBufferIsValid(sampleBuffer),
              let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer)
        else { return }
        delivery.submit(stream: stream, buffer: pixelBuffer)
    }

    nonisolated func stream(_ stream: SCStream, didStopWithError error: Error) {
        let streamIdentifier = ObjectIdentifier(stream)
        let message = error.localizedDescription
        Task { @MainActor [weak self, streamIdentifier, message] in
            guard let self,
                  self.stream.map({ ObjectIdentifier($0) }) == streamIdentifier
            else { return }
            stopHandler?(ScreenCaptureSourceError.streamStopped(message))
        }
    }
}

enum ScreenCaptureSourceError: Error, Equatable {
    case displayUnavailable
    case streamStopped(String)
}
