import CoreVideo
import Foundation

public struct H264DecodedFrame: @unchecked Sendable {
    public let generation: UInt64
    public let streamID: UInt64
    public let inputEpoch: UInt64
    public let version: UInt32
    public let width: UInt16
    public let height: UInt16
    public let frameID: UInt64
    public let pixelBuffer: CVPixelBuffer
}

/// One mailbox for all decoder-to-UI work. A stalled main actor retains only
/// the newest output/configuration/request instead of one closure per packet.
private final class DecodeOutputDelivery: @unchecked Sendable {
    struct Configuration: Sendable {
        let generation: UInt64
        let streamID: UInt64
        let inputEpoch: UInt64
        let version: UInt32
        let width: UInt16
        let height: UInt16
    }

    private let lock = NSLock()
    private var frame: H264DecodedFrame?
    private var configuration: Configuration?
    private var keyframe: (UInt64, UInt64)?
    private var scheduled = false
    private let onFrame: @MainActor @Sendable (H264DecodedFrame) -> Void
    private let onConfiguration: @MainActor @Sendable (UInt64, UInt64, UInt64, UInt32, UInt16, UInt16) -> Void
    private let onKeyframeRequest: @MainActor @Sendable (UInt64, UInt64) -> Void

    init(
        onFrame: @escaping @MainActor @Sendable (H264DecodedFrame) -> Void,
        onConfiguration: @escaping @MainActor @Sendable (UInt64, UInt64, UInt64, UInt32, UInt16, UInt16) -> Void,
        onKeyframeRequest: @escaping @MainActor @Sendable (UInt64, UInt64) -> Void
    ) {
        self.onFrame = onFrame
        self.onConfiguration = onConfiguration
        self.onKeyframeRequest = onKeyframeRequest
    }

    func clear() {
        lock.lock()
        frame = nil
        configuration = nil
        keyframe = nil
        lock.unlock()
    }

    func submit(frame: H264DecodedFrame? = nil, configuration: Configuration? = nil, keyframe: (UInt64, UInt64)? = nil) {
        lock.lock()
        if let frame { self.frame = frame }
        if let configuration { self.configuration = configuration }
        if let keyframe { self.keyframe = keyframe }
        let shouldSchedule = !scheduled
        scheduled = true
        lock.unlock()
        guard shouldSchedule else { return }
        Task { @MainActor [weak self] in
            guard let self else { return }
            while let next = take() {
                if let configuration = next.1 {
                    onConfiguration(configuration.generation, configuration.streamID, configuration.inputEpoch, configuration.version,
                                    configuration.width, configuration.height)
                }
                if let frame = next.0 { onFrame(frame) }
                if let keyframe = next.2 { onKeyframeRequest(keyframe.0, keyframe.1) }
                await Task.yield()
            }
        }
    }

    private func take() -> (H264DecodedFrame?, Configuration?, (UInt64, UInt64)?)? {
        lock.lock()
        defer { lock.unlock() }
        guard frame != nil || configuration != nil || keyframe != nil else {
            scheduled = false
            return nil
        }
        let next = (frame, configuration, keyframe)
        frame = nil
        configuration = nil
        keyframe = nil
        return next
    }
}

/// Thread-safe FFI ingress. All native configuration, conversion and decoding
/// run on one queue; only coalesced output and state reach the main actor.
public final class H264DecodeScheduler: @unchecked Sendable {
    public static let maximumEncodedInputBytes = 8 * 1_024 * 1_024
    public static let maximumSequenceHeaderBytes = 64 * 1_024

    public struct Limits: Sendable {
        public let frames: Int
        public let bytes: Int
        public let ageNanoseconds: UInt64

        public init(frames: Int = 6, bytes: Int = H264DecodeScheduler.maximumEncodedInputBytes,
                    ageNanoseconds: UInt64 = 250_000_000) {
            self.frames = max(1, frames)
            self.bytes = max(1, min(bytes, H264DecodeScheduler.maximumEncodedInputBytes))
            self.ageNanoseconds = max(1, ageNanoseconds)
        }
    }

    public struct Snapshot: Sendable {
        public let queuedFrames: Int
        public let queuedBytes: Int
        public let awaitingKeyframe: Bool
        public let droppedFrames: UInt64
        public let decodedFrames: UInt64
    }

    private struct Configuration: Sendable {
        let generation: UInt64
        let epoch: UInt64
        let streamID: UInt64
        let inputEpoch: UInt64
        let version: UInt32
        let width: UInt16
        let height: UInt16
        let sequenceHeader: Data
    }

    private struct Frame: Sendable {
        let configuration: Configuration
        let frameID: UInt64
        let arrivedAt: UInt64
        let accessUnit: Data
    }

    private enum Work {
        case reset
        case configure(Configuration)
        case frame(Frame)
    }

    private let queue: DispatchQueue
    private let limits: Limits
    private let lock = NSLock()
    private let decoder = H264DecodeSession()
    private let delivery: DecodeOutputDelivery
    private var generation: UInt64 = 0
    private var epoch: UInt64 = 0
    private var configuration: Configuration?
    private var pendingConfiguration: Configuration?
    private var pendingReset = false
    private var frames: [Frame] = []
    private var queuedBytes = 0
    private var droppedFrames: UInt64 = 0
    private var decodedFrames: UInt64 = 0
    private var lastAdmittedFrameID: UInt64 = 0
    private var awaitingKeyframe = true
    private var keyframeRequestIssued = false
    private var draining = false
    private var inputSurfaceContractViolated = false

    public init(
        limits: Limits = Limits(),
        decodeQueue: DispatchQueue = DispatchQueue(label: "com.desklink.decoder.input", qos: .userInitiated),
        onFrame: @escaping @MainActor @Sendable (H264DecodedFrame) -> Void,
        onConfiguration: @escaping @MainActor @Sendable (UInt64, UInt64, UInt64, UInt32, UInt16, UInt16) -> Void,
        onKeyframeRequest: @escaping @MainActor @Sendable (UInt64, UInt64) -> Void
    ) {
        self.limits = limits
        queue = decodeQueue
        delivery = DecodeOutputDelivery(onFrame: onFrame, onConfiguration: onConfiguration,
                                        onKeyframeRequest: onKeyframeRequest)
    }

    /// Call once when the FFI callback context generation changes, before
    /// accepting callbacks. Stream/config changes are handled by configure.
    public func reset(generation: UInt64) {
        lock.lock()
        defer { lock.unlock() }
        resetLocked(generation: generation)
    }

    /// A reconnect notification is handled synchronously at FFI ingress. An
    /// already retired callback must never reset the successor handle's queue.
    @discardableResult
    public func retireStream(generation: UInt64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard generation == self.generation else { return false }
        resetLocked(generation: generation)
        return true
    }

    private func resetLocked(generation: UInt64) {
        self.generation = generation
        epoch &+= 1
        configuration = nil
        pendingConfiguration = nil
        discardPendingFrames()
        lastAdmittedFrameID = 0
        awaitingKeyframe = true
        keyframeRequestIssued = false
        inputSurfaceContractViolated = false
        pendingReset = true
        delivery.clear()
        scheduleDrain()
    }

    public func configure(sequenceHeader: Data, streamID: UInt64, inputEpoch: UInt64, width: UInt16, height: UInt16,
                          version: UInt32, generation: UInt64) {
        guard streamID > 0, inputEpoch > 0, width > 0, height > 0, version > 0,
              !sequenceHeader.isEmpty, sequenceHeader.count <= Self.maximumSequenceHeaderBytes else { return }
        lock.lock()
        defer { lock.unlock() }
        guard generation == self.generation, !inputSurfaceContractViolated else { return }
        if let current = configuration {
            guard streamID > current.streamID || (streamID == current.streamID && version >= current.version) else { return }
            if streamID == current.streamID {
                // Input epochs are opaque nonzero identities. A restarted host
                // may choose a numerically smaller one; only equality matters.
                if inputEpoch == current.inputEpoch, width != current.width || height != current.height {
                    resetLocked(generation: generation)
                    inputSurfaceContractViolated = true
                    return
                }
                if version == current.version { return }
            }
        }
        epoch &+= 1
        let next = Configuration(generation: generation, epoch: epoch, streamID: streamID, inputEpoch: inputEpoch,
                                 version: version, width: width, height: height, sequenceHeader: sequenceHeader)
        configuration = next
        pendingConfiguration = next
        pendingReset = true
        discardPendingFrames()
        lastAdmittedFrameID = 0
        awaitingKeyframe = true
        keyframeRequestIssued = false
        delivery.clear()
        // This is a configuration admission barrier, not a decode-success
        // notification. Invalidate the old input context even if native decode
        // is busy; input becomes valid only after an actual drawable receipt.
        delivery.submit(configuration: .init(generation: next.generation, streamID: next.streamID, inputEpoch: next.inputEpoch,
                        version: next.version, width: next.width, height: next.height))
        scheduleDrain()
    }

    public func receive(accessUnit: Data, streamID: UInt64, frameID: UInt64, version: UInt32, generation: UInt64) {
        guard frameID > 0, frameID <= UInt64(Int64.max),
              !accessUnit.isEmpty, accessUnit.count <= limits.bytes else {
            invalidateDependency(generation: generation, streamID: streamID, version: version)
            return
        }
        let isKeyframe = H264AnnexB.containsIDR(in: accessUnit)
        let arrivedAt = DispatchTime.now().uptimeNanoseconds
        lock.lock()
        defer { lock.unlock() }
        guard generation == self.generation, let current = configuration,
              streamID == current.streamID, version == current.version,
              frameID > lastAdmittedFrameID else { return }
        let expired = frames.first.map { arrivedAt &- $0.arrivedAt > limits.ageNanoseconds } ?? false
        if expired || frames.count >= limits.frames || queuedBytes > limits.bytes - accessUnit.count {
            discardPendingFrames()
            awaitingKeyframe = true
        }
        if awaitingKeyframe, !isKeyframe {
            droppedFrames &+= 1
            requestKeyframe(current)
            return
        }
        if isKeyframe {
            awaitingKeyframe = false
            keyframeRequestIssued = false
        }
        frames.append(Frame(configuration: current, frameID: frameID, arrivedAt: arrivedAt, accessUnit: accessUnit))
        queuedBytes += accessUnit.count
        lastAdmittedFrameID = frameID
        scheduleDrain()
    }

    /// Validate coalesced UI callbacks again when they run: reset/configure can
    /// happen after the mailbox was taken but before the main actor executes it.
    public func accepts(generation: UInt64, streamID: UInt64, version: UInt32, inputEpoch: UInt64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return inputEpoch > 0 && generation == self.generation && configuration?.streamID == streamID
            && configuration?.version == version && configuration?.inputEpoch == inputEpoch
    }

    /// Capture may retain a genuinely presented frame across a pure encoding
    /// reconfiguration. Same-epoch dimension changes are rejected at admission.
    public func acceptsInputSurface(generation: UInt64, streamID: UInt64, inputEpoch: UInt64) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return inputEpoch > 0 && generation == self.generation && configuration?.streamID == streamID
            && configuration?.inputEpoch == inputEpoch
    }

    public func currentInputContext(generation: UInt64) -> CapturedInputContext? {
        lock.lock()
        defer { lock.unlock() }
        guard generation == self.generation, let current = configuration else { return nil }
        return CapturedInputContext(streamID: current.streamID, inputEpoch: current.inputEpoch,
                                    configVersion: current.version, runtimeGeneration: generation)
    }

    public func snapshot() -> Snapshot {
        lock.lock()
        defer { lock.unlock() }
        return Snapshot(queuedFrames: frames.count, queuedBytes: queuedBytes,
                        awaitingKeyframe: awaitingKeyframe, droppedFrames: droppedFrames, decodedFrames: decodedFrames)
    }

    /// Must be called with lock held. Only one drain closure can be scheduled.
    private func scheduleDrain() {
        guard !draining else { return }
        draining = true
        queue.async { [weak self] in self?.drain() }
    }

    private func nextWork() -> Work? {
        lock.lock()
        defer { lock.unlock() }
        if pendingReset {
            pendingReset = false
            return .reset
        }
        if let pending = pendingConfiguration {
            pendingConfiguration = nil
            return .configure(pending)
        }
        if let next = frames.first {
            if DispatchTime.now().uptimeNanoseconds &- next.arrivedAt > limits.ageNanoseconds {
                discardPendingFrames()
                awaitingKeyframe = true
                requestKeyframe(next.configuration)
            } else {
                frames.removeFirst()
                queuedBytes -= next.accessUnit.count
                return .frame(next)
            }
        }
        draining = false
        return nil
    }

    private func drain() {
        while let work = nextWork() {
            switch work {
            case .reset:
                decoder.reset()
            case let .configure(config):
                guard isCurrent(config) else { continue }
                guard decoder.configure(sequenceHeader: config.sequenceHeader, width: config.width,
                                        height: config.height, version: config.version) else {
                    invalidateDependency(generation: config.generation, streamID: config.streamID, version: config.version)
                    continue
                }
                // The native session has no outstanding callbacks at this point.
                // Freeze identity into its handler so old-session output cannot be
                // relabelled as a successor stream/configuration.
                decoder.onFrame = { [weak self] buffer, frameID in
                    self?.publishFrame(buffer, frameID: frameID, configuration: config)
                }
            case let .frame(frame):
                guard isCurrent(frame.configuration) else { continue }
                if !decoder.receive(accessUnit: frame.accessUnit, frameID: frame.frameID, version: frame.configuration.version) {
                    invalidateDependency(generation: frame.configuration.generation,
                                         streamID: frame.configuration.streamID, version: frame.configuration.version)
                }
            }
        }
    }

    private func publishFrame(_ buffer: CVPixelBuffer, frameID: UInt64, configuration config: Configuration) {
        lock.lock()
        defer { lock.unlock() }
        guard generation == config.generation, epoch == config.epoch else { return }
        decodedFrames &+= 1
        delivery.submit(frame: H264DecodedFrame(generation: config.generation, streamID: config.streamID, inputEpoch: config.inputEpoch,
                        version: config.version, width: config.width, height: config.height, frameID: frameID, pixelBuffer: buffer))
    }

    private func isCurrent(_ config: Configuration) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return generation == config.generation && epoch == config.epoch
    }

    private func invalidateDependency(generation: UInt64, streamID: UInt64, version: UInt32) {
        lock.lock()
        defer { lock.unlock() }
        guard self.generation == generation, let current = configuration,
              current.streamID == streamID, current.version == version else { return }
        discardPendingFrames()
        awaitingKeyframe = true
        requestKeyframe(current)
    }

    private func discardPendingFrames() {
        droppedFrames &+= UInt64(frames.count)
        frames.removeAll(keepingCapacity: true)
        queuedBytes = 0
    }

    private func requestKeyframe(_ config: Configuration) {
        guard !keyframeRequestIssued else { return }
        keyframeRequestIssued = true
        delivery.submit(keyframe: (config.generation, config.streamID))
    }
}
