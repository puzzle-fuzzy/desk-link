import CoreGraphics
import CoreVideo
import Foundation

/// Immutable identity captured with the pixels submitted to Metal. A decode
/// callback alone cannot confirm this token; only a drawable presentation can.
public struct VideoFramePresentationToken: Equatable, Hashable, Sendable {
    public let context: CapturedInputContext
    public let frameID: UInt64
    public let width: UInt16
    public let height: UInt16

    public init(context: CapturedInputContext, frameID: UInt64, width: UInt16, height: UInt16) {
        self.context = context
        self.frameID = frameID
        self.width = width
        self.height = height
    }

    public var videoSize: CGSize { CGSize(width: Int(width), height: Int(height)) }
}

/// Pixels and their identity travel together through SwiftUI and the renderer.
/// Supplying this value to a view does not yet enable remote input.
public struct PresentedVideoFrame: @unchecked Sendable {
    public let pixelBuffer: CVPixelBuffer
    public let token: VideoFramePresentationToken

    public init(pixelBuffer: CVPixelBuffer, token: VideoFramePresentationToken) {
        self.pixelBuffer = pixelBuffer
        self.token = token
    }
}

/// Pure acknowledgement state. The bridge supplies the current scheduler
/// identity and highest issued frame after checking its live runtime/state.
struct VideoPresentationState {
    private(set) var context: CapturedInputContext?
    private(set) var frameID: UInt64 = 0

    mutating func reset() {
        context = nil
        frameID = 0
    }

    mutating func confirm(_ token: VideoFramePresentationToken, current: CapturedInputContext,
                          newestIssuedFrameID: UInt64) -> Bool {
        guard token.context == current, current.inputEpoch > 0, current.streamID > 0,
              current.configVersion > 0, token.frameID > 0, token.frameID <= newestIssuedFrameID,
              token.width > 0, token.height > 0 else { return false }
        if context == current, token.frameID < frameID { return false }
        context = current
        frameID = token.frameID
        return true
    }
}
