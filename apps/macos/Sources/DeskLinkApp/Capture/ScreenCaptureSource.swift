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

final class ScreenCaptureSource: NSObject, SCStreamOutput, SCStreamDelegate, @unchecked Sendable {
    typealias FrameHandler = @Sendable (CVPixelBuffer, UInt64) -> Void
    typealias StopHandler = @Sendable (Error) -> Void

    private let sampleQueue = DispatchQueue(label: "com.desklink.capture.sample", qos: .userInteractive)
    private let deliveryQueue = DispatchQueue(label: "com.desklink.capture.delivery", qos: .userInitiated)
    private var stream: SCStream?
    private var frameID: UInt64 = 0
    private var selectedStreamID: UInt64 = 0
    private var frameHandler: FrameHandler?
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
        let content = try await SCShareableContent.excludingDesktopWindows(
            false,
            onScreenWindowsOnly: true
        )
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
        self.stream = stream
        frameID = 0
        selectedStreamID = streamID
        capturedDisplayFrame = display.frame
        frameHandler = onFrame
        stopHandler = onStop
        do {
            try await stream.startCapture()
        } catch {
            self.stream = nil
            frameHandler = nil
            stopHandler = nil
            throw error
        }
    }

    func stop() async {
        let stream = stream
        self.stream = nil
        frameHandler = nil
        stopHandler = nil
        frameID = 0
        selectedStreamID = 0
        capturedDisplayFrame = .zero
        if let stream { try? await stream.stopCapture() }
    }

    func stream(
        _ stream: SCStream,
        didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of outputType: SCStreamOutputType
    ) {
        guard outputType == .screen,
              CMSampleBufferIsValid(sampleBuffer),
              let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer)
        else { return }
        frameID &+= 1
        let nextFrameID = frameID
        let buffer = CapturedPixelBuffer(pixelBuffer)
        deliveryQueue.async { [weak self, buffer] in
            self?.frameHandler?(buffer.value, nextFrameID)
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        deliveryQueue.async { [weak self] in
            self?.stopHandler?(error)
        }
    }
}

enum ScreenCaptureSourceError: Error, Equatable {
    case displayUnavailable
}
