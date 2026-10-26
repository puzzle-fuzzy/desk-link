import Combine
import Foundation

/// The protocol format is fixed by the authenticated Rust decoder. No capture
/// device is opened by this playback path.
enum RemotePCM {
    static let sampleRate = 48_000.0
    static let samplesPerFrame = 480
    static let bytesPerFrame = samplesPerFrame * MemoryLayout<Int16>.size

    static func samples(from data: Data) -> [Float]? {
        guard data.count == bytesPerFrame else { return nil }
        return data.withUnsafeBytes { bytes in
            (0..<samplesPerFrame).map { index in
                let low = UInt16(bytes[index * 2])
                let high = UInt16(bytes[index * 2 + 1]) << 8
                return Float(Int16(bitPattern: low | high)) / 32_768
            }
        }
    }
}

/// All methods run on one audio scheduling queue. Completion may arrive from
/// any thread and must never directly manipulate the native player node.
protocol RemoteAudioOutput: AnyObject, Sendable {
    func start(volume: Float) throws
    func schedule(samples: [Float], completion: @escaping @Sendable () -> Void) throws
    func play()
    func setVolume(_ volume: Float)
    func stop()
}

@MainActor
final class RemoteAudioPlayer: ObservableObject {
    @Published private(set) var volume: Double = 0.8
    @Published private(set) var isMuted = false
    @Published private(set) var isPlaying = false
    @Published private(set) var localError: String?

    private var sessionStreamID: UInt64?
    private var sessionEnabled = false
    private var callbackGeneration: UInt64 = 0
    private let scheduler: RemoteAudioScheduler

    init(output: any RemoteAudioOutput = NativeRemoteAudioOutput()) {
        scheduler = RemoteAudioScheduler(output: output)
        scheduler.onFailure = { [weak self] generation in
            Task { @MainActor [weak self] in
                guard let self, self.callbackGeneration == generation else { return }
                self.isPlaying = false
                self.localError = "无法播放远端声音，请检查本机音频输出后重新开启声音。"
            }
        }
        scheduler.onPlaying = { [weak self] generation in
            Task { @MainActor [weak self] in
                guard let self, self.callbackGeneration == generation, self.sessionEnabled else { return }
                self.isPlaying = true
            }
        }
    }

    /// Call only after a positive host acknowledgement. Changing stream or
    /// disabling immediately invalidates queued packets and native callbacks.
    func setSession(streamID: UInt64?, enabled: Bool) {
        let activeID = enabled ? streamID : nil
        let active = enabled && activeID != nil
        guard sessionStreamID != activeID || sessionEnabled != active else { return }
        sessionStreamID = activeID
        sessionEnabled = active
        callbackGeneration &+= 1
        isPlaying = false
        localError = nil
        scheduler.configure(streamID: activeID, generation: callbackGeneration,
                            volume: effectiveVolume)
    }

    func receive(streamID: UInt64, sequence: UInt64, pcm: Data) {
        guard sessionEnabled, sessionStreamID == streamID else { return }
        scheduler.receive(streamID: streamID, sequence: sequence, pcm: pcm)
    }

    func setVolume(_ value: Double) {
        guard value.isFinite else { return }
        volume = min(1, max(0, value))
        scheduler.setVolume(effectiveVolume)
    }

    func setMuted(_ muted: Bool) {
        isMuted = muted
        scheduler.setVolume(effectiveVolume)
    }

    func stop() { setSession(streamID: nil, enabled: false) }

    private var effectiveVolume: Float { isMuted ? 0 : Float(volume) }

    #if DEBUG
    func waitUntilIdleForTesting() async { await scheduler.waitUntilIdle() }
    var audioQueueSnapshotForTesting: RemoteAudioQueueSnapshot { scheduler.snapshot() }
    #endif
}

struct RemoteAudioQueueSnapshot: Sendable {
    let queuedFrames: Int
    let droppedFrames: UInt64
    let rejectedFrames: UInt64
}

/// A bounded ingress mailbox prevents a congested UI or device from building
/// an unbounded dispatch backlog. At most 6 waiting + 4 scheduled frames (100
/// ms) are retained; old packets are discarded to keep sound near the picture.
private final class RemoteAudioScheduler: @unchecked Sendable {
    private struct Frame { let pcm: Data }
    private struct Mailbox {
        var streamID: UInt64?
        var generation: UInt64 = 0
        var lastSequence: UInt64?
        var frames: [Frame] = []
        var drainQueued = false
        var dropped: UInt64 = 0
        var rejected: UInt64 = 0
    }

    static let maximumWaitingFrames = 6
    static let maximumScheduledFrames = 4
    private let lock = NSLock()
    private var mailbox = Mailbox()
    private let queue = DispatchQueue(label: "com.desklink.remote-audio", qos: .userInteractive)
    private let queueKey = DispatchSpecificKey<Bool>()
    private let output: any RemoteAudioOutput
    // The following state is confined to queue.
    private var outputGeneration: UInt64 = 0
    private var scheduledFrames = 0
    private var started = false
    private var notifiedPlaying = false
    private var volume: Float = 0.8
    var onFailure: (@Sendable (UInt64) -> Void)?
    var onPlaying: (@Sendable (UInt64) -> Void)?

    init(output: any RemoteAudioOutput) {
        self.output = output
        queue.setSpecific(key: queueKey, value: true)
    }

    deinit {
        if DispatchQueue.getSpecific(key: queueKey) == true { output.stop() }
        else { queue.sync { output.stop() } }
    }

    func configure(streamID: UInt64?, generation: UInt64, volume: Float) {
        lock.withLock {
            mailbox.streamID = streamID
            mailbox.generation = generation
            mailbox.frames.removeAll(keepingCapacity: true)
            mailbox.lastSequence = nil
            mailbox.drainQueued = false
        }
        queue.async { [self] in
            output.stop()
            outputGeneration = generation
            scheduledFrames = 0
            started = false
            notifiedPlaying = false
            self.volume = volume
        }
    }

    func setVolume(_ value: Float) {
        queue.async { [self] in
            volume = value
            if started { output.setVolume(value) }
        }
    }

    func receive(streamID: UInt64, sequence: UInt64, pcm: Data) {
        let generation: UInt64? = lock.withLock {
            guard mailbox.streamID == streamID, pcm.count == RemotePCM.bytesPerFrame,
                  mailbox.lastSequence.map({ sequence > $0 }) ?? true else {
                mailbox.rejected &+= 1
                return nil
            }
            mailbox.lastSequence = sequence
            if mailbox.frames.count == Self.maximumWaitingFrames {
                mailbox.frames.removeFirst()
                mailbox.dropped &+= 1
            }
            mailbox.frames.append(Frame(pcm: pcm))
            guard !mailbox.drainQueued else { return nil }
            mailbox.drainQueued = true
            return mailbox.generation
        }
        if let generation { queue.async { [self] in drain(generation: generation) } }
    }

    private func drain(generation: UInt64) {
        guard outputGeneration == generation else { return }
        let frames: [Frame] = lock.withLock {
            guard mailbox.generation == generation, mailbox.streamID != nil else { return [] }
            mailbox.drainQueued = false
            let room = Self.maximumScheduledFrames - scheduledFrames
            // Two packets form a small startup/underflow jitter cushion.
            guard scheduledFrames > 0 || mailbox.frames.count >= 2 else { return [] }
            let count = min(room, mailbox.frames.count)
            let result = Array(mailbox.frames.prefix(count))
            mailbox.frames.removeFirst(count)
            return result
        }
        guard !frames.isEmpty else { return }
        do {
            if !started {
                try output.start(volume: volume)
                started = true
            }
            for frame in frames {
                guard isCurrent(generation) else { return }
                guard let samples = RemotePCM.samples(from: frame.pcm) else { continue }
                scheduledFrames += 1
                try output.schedule(samples: samples) { [weak self] in
                    guard let self else { return }
                    self.queue.async { [self] in completed(generation: generation) }
                }
            }
            output.play()
            if !notifiedPlaying {
                notifiedPlaying = true
                onPlaying?(generation)
            }
        } catch {
            fail(generation: generation)
        }
    }

    private func completed(generation: UInt64) {
        guard outputGeneration == generation, isCurrent(generation) else { return }
        scheduledFrames = max(0, scheduledFrames - 1)
        drain(generation: generation)
    }

    private func isCurrent(_ generation: UInt64) -> Bool {
        lock.withLock { mailbox.generation == generation && mailbox.streamID != nil }
    }

    private func fail(generation: UInt64) {
        let current = lock.withLock {
            guard mailbox.generation == generation else { return false }
            mailbox.streamID = nil
            mailbox.frames.removeAll(keepingCapacity: true)
            mailbox.drainQueued = false
            return true
        }
        output.stop()
        started = false
        scheduledFrames = 0
        if current { onFailure?(generation) }
    }

    func snapshot() -> RemoteAudioQueueSnapshot {
        lock.withLock {
            RemoteAudioQueueSnapshot(queuedFrames: mailbox.frames.count,
                                     droppedFrames: mailbox.dropped,
                                     rejectedFrames: mailbox.rejected)
        }
    }

    func waitUntilIdle() async {
        await withCheckedContinuation { continuation in
            queue.async { continuation.resume() }
        }
    }
}
