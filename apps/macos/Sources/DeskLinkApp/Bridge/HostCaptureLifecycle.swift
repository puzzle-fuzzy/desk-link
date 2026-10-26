import CoreGraphics
import CoreVideo
import DeskLinkAppleCore
import Foundation

enum HostCaptureConnectionPhase: Equatable, Sendable {
    case connected
    case unavailable
    case terminal
}

struct HostCaptureConnectionReceipt: Equatable, Sendable {
    let runtimeGeneration: UInt64
    let sequence: UInt64
    let mediaGeneration: UInt64
    let phase: HostCaptureConnectionPhase
}

struct HostCaptureOwner: Equatable, Hashable, Sendable {
    let runtimeGeneration: UInt64
    let connectionSequence: UInt64
    let mediaGeneration: UInt64
    let streamID: UInt64
}

struct OwnedHostVideoEvent: Equatable, Sendable {
    let owner: HostCaptureOwner
    let event: EncodedVideoEvent
}

/// Original Rust input identity. Swift does not allocate or learn a current
/// epoch from an input callback; Rust validates it against published geometry.
struct HostInputContext: Equatable, Sendable {
    let streamID: UInt64
    let inputEpoch: UInt64
}

struct HostInputDelivery: Sendable {
    let admission: HostCaptureAdmission
    let connectionReceipt: HostCaptureConnectionReceipt
    let captureOwner: HostCaptureOwner
    let mediaGeneration: UInt64
    let context: HostInputContext
    let command: RemoteInputCommand
}

/// Shared by the native callback thread and final MainActor admission. Each
/// native handle gets its own instance; callbacks from a retired handle cannot
/// alter the successor's capture permission.
final class HostCaptureAdmission: @unchecked Sendable {
    private let lock = NSLock()
    private let runtimeGeneration: UInt64
    private var sequence: UInt64 = 0
    private var greatestMediaGeneration: UInt64 = 0
    private var phase: HostCaptureConnectionPhase = .unavailable
    private var closed = false
    private var capture: HostCaptureOwner?

    init(runtimeGeneration: UInt64) { self.runtimeGeneration = runtimeGeneration }

    /// Called synchronously in the native state callback, before dispatching
    /// any UI notification. Rust's scoped submit independently verifies the
    /// Noise-session media generation, including time before this callback ran.
    func observe(_ next: HostCaptureConnectionPhase, mediaGeneration: UInt64) -> HostCaptureConnectionReceipt? {
        lock.lock()
        defer { lock.unlock() }
        // Stopping seals capture admission, but its following Closed callback
        // must still reach teardown. A sealed gate can never reopen capture.
        guard !closed || next == .terminal else { return nil }
        // A callback already stamped for an older Noise cannot retire a newer
        // capture. Zero denotes handle-level state without a Noise receipt.
        guard mediaGeneration == 0 || mediaGeneration >= greatestMediaGeneration else { return nil }
        if next == .connected {
            guard mediaGeneration > 0 else { return nil }
            if phase == .connected, mediaGeneration == greatestMediaGeneration { return receipt() }
            guard mediaGeneration > greatestMediaGeneration else { return nil }
            greatestMediaGeneration = mediaGeneration
        }
        guard sequence < UInt64.max else {
            closed = true
            capture = nil
            return nil
        }
        sequence += 1
        phase = next
        capture = nil
        if next == .terminal { closed = true }
        return receipt()
    }

    func matches(_ value: HostCaptureConnectionReceipt) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return value == receipt()
    }

    func register(_ value: HostCaptureConnectionReceipt, streamID: UInt64) -> HostCaptureOwner? {
        lock.lock()
        defer { lock.unlock() }
        guard !closed, phase == .connected, value == receipt(), streamID > 0, capture == nil else { return nil }
        let owner = HostCaptureOwner(runtimeGeneration: runtimeGeneration, connectionSequence: sequence,
                                     mediaGeneration: greatestMediaGeneration, streamID: streamID)
        capture = owner
        return owner
    }

    func allows(_ owner: HostCaptureOwner) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return permits(owner)
    }

    /// Serializes Swift resource retirement with a bounded FFI queue admission.
    /// This lock is not a substitute for Rust's per-Noise generation validation.
    func withAdmission<T>(_ owner: HostCaptureOwner, _ body: () -> T) -> T? {
        lock.lock()
        defer { lock.unlock() }
        guard permits(owner) else { return nil }
        return body()
    }

    /// Snapshots the existing capture grant on the native callback thread.
    /// Never call observe/register here or adopt a callback's epoch as current.
    func captureInput(mediaGeneration: UInt64, context: HostInputContext,
                      command: RemoteInputCommand) -> HostInputDelivery? {
        lock.lock()
        defer { lock.unlock() }
        guard let capture, permits(capture), mediaGeneration > 0,
              mediaGeneration == capture.mediaGeneration,
              context.streamID > 0, context.streamID == capture.streamID,
              context.inputEpoch > 0 else { return nil }
        return HostInputDelivery(admission: self, connectionReceipt: receipt(), captureOwner: capture,
                                 mediaGeneration: mediaGeneration, context: context, command: command)
    }

    /// Final synchronous OS admission. No network/crypto lock is held. Native
    /// retirement cannot run between this check and the local backend call.
    func withInputAdmission<T>(_ input: HostInputDelivery, _ body: () -> T) -> T? {
        lock.lock()
        defer { lock.unlock() }
        guard input.admission === self, input.connectionReceipt == receipt(),
              permits(input.captureOwner), input.mediaGeneration == input.captureOwner.mediaGeneration,
              input.context.streamID == input.captureOwner.streamID,
              input.context.inputEpoch > 0 else { return nil }
        return body()
    }

    func retire(_ owner: HostCaptureOwner) {
        lock.lock()
        if capture == owner { capture = nil }
        lock.unlock()
    }

    func close() {
        lock.lock()
        closed = true
        capture = nil
        phase = .terminal
        lock.unlock()
    }

    private func permits(_ owner: HostCaptureOwner) -> Bool {
        !closed && phase == .connected && capture == owner
    }

    private func receipt() -> HostCaptureConnectionReceipt {
        HostCaptureConnectionReceipt(runtimeGeneration: runtimeGeneration, sequence: sequence,
                                     mediaGeneration: greatestMediaGeneration, phase: phase)
    }
}

struct HostCaptureGeometry: Equatable, Sendable {
    let displayFrame: CGRect
    let videoSize: CGSize
}

struct HostCaptureCallbacks: Sendable {
    let video: @Sendable (EncodedVideoEvent) -> Void
    let encodingError: @Sendable (MacH264EncoderError) -> Void
    let captureStopped: @Sendable (String) -> Void
}

@MainActor
protocol HostCaptureSession: AnyObject {
    func start() async throws -> HostCaptureGeometry
    func stop() async
    func requestKeyframe()
}

typealias HostCaptureFactory = @MainActor @Sendable (HostCaptureOwner, HostCaptureCallbacks) -> any HostCaptureSession

/// The default testing factory must never request capture or media permissions.
@MainActor
final class InertHostCaptureSession: HostCaptureSession {
    func start() async throws -> HostCaptureGeometry { throw CancellationError() }
    func stop() async {}
    func requestKeyframe() {}
}

/// Resources and handlers belong to exactly one capture attempt. No old stop
/// task can flush, invalidate, or replace a successor's encoder.
@MainActor
final class NativeHostCaptureSession: HostCaptureSession {
    private let source = ScreenCaptureSource()
    private let encoder = MacH264Encoder()
    private let owner: HostCaptureOwner
    private let callbacks: HostCaptureCallbacks

    init(owner: HostCaptureOwner, callbacks: HostCaptureCallbacks) {
        self.owner = owner
        self.callbacks = callbacks
        encoder.onEvent = callbacks.video
        encoder.onError = callbacks.encodingError
    }

    func start() async throws -> HostCaptureGeometry {
        try Task.checkCancellation()
        try await source.start(
            displayID: CGMainDisplayID(), streamID: owner.streamID,
            onFrame: { [weak encoder] buffer, frameID in encoder?.encode(pixelBuffer: buffer, frameID: frameID) },
            onStop: { [callbacks] error in callbacks.captureStopped(error.localizedDescription) }
        )
        try Task.checkCancellation()
        let geometry = HostCaptureGeometry(displayFrame: source.capturedDisplayFrame, videoSize: source.capturedVideoSize)
        guard geometry.videoSize.width > 0, geometry.videoSize.height > 0 else { throw ScreenCaptureSourceError.displayUnavailable }
        try encoder.start(width: Int(geometry.videoSize.width), height: Int(geometry.videoSize.height), streamID: owner.streamID)
        return geometry
    }

    func stop() async {
        await source.stop()
        encoder.stop()
    }

    func requestKeyframe() { encoder.requestKeyframe() }
}

@MainActor
final class HostCaptureAttempt {
    let owner: HostCaptureOwner
    let admission: HostCaptureAdmission
    let session: any HostCaptureSession
    var startTask: Task<Void, Never>?

    init(owner: HostCaptureOwner, admission: HostCaptureAdmission, session: any HostCaptureSession) {
        self.owner = owner
        self.admission = admission
        self.session = session
    }
}
