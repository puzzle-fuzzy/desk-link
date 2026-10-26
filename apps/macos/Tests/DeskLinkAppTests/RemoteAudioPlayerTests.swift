import AVFAudio
import Foundation
import XCTest
@testable import DeskLinkApp

@MainActor
final class RemoteAudioPlayerTests: XCTestCase {
    func testPCMRequiresExactFrameAndDecodesSignedLittleEndian() {
        XCTAssertNil(RemotePCM.samples(from: Data(count: 959)))
        XCTAssertNil(RemotePCM.samples(from: Data(count: 961)))
        var pcm = Data([0, 128, 255, 127, 0, 0])
        pcm.append(Data(count: RemotePCM.bytesPerFrame - pcm.count))
        let samples = RemotePCM.samples(from: pcm)!
        XCTAssertEqual(samples.count, 480)
        XCTAssertEqual(samples[0], -1)
        XCTAssertEqual(samples[1], Float(32_767) / 32_768)
        XCTAssertEqual(samples[2], 0)
    }

    func testDoesNotStartWithoutExplicitEnabledMatchingSession() async {
        let output = RecordingAudioOutput()
        let player = RemoteAudioPlayer(output: output)
        player.receive(streamID: 42, sequence: 1, pcm: silence)
        player.setSession(streamID: 42, enabled: false)
        player.receive(streamID: 42, sequence: 2, pcm: silence)
        player.setSession(streamID: 42, enabled: true)
        player.receive(streamID: 99, sequence: 3, pcm: silence)
        await player.waitUntilIdleForTesting()
        XCTAssertEqual(output.state.starts, 0)
        XCTAssertEqual(output.state.scheduled.count, 0)
    }

    func testSmallStartupCushionAndMalformedOrOutOfOrderPackets() async {
        let output = RecordingAudioOutput()
        let player = RemoteAudioPlayer(output: output)
        player.setSession(streamID: 42, enabled: true)
        player.receive(streamID: 42, sequence: 10, pcm: silence)
        await player.waitUntilIdleForTesting()
        XCTAssertEqual(output.state.starts, 0)
        player.receive(streamID: 42, sequence: 11, pcm: Data(count: 959))
        player.receive(streamID: 42, sequence: 10, pcm: silence)
        player.receive(streamID: 42, sequence: 9, pcm: silence)
        player.receive(streamID: 42, sequence: 12, pcm: silence)
        await player.waitUntilIdleForTesting()
        XCTAssertEqual(output.state.starts, 1)
        XCTAssertEqual(output.state.scheduled.count, 2)
        XCTAssertEqual(player.audioQueueSnapshotForTesting.rejectedFrames, 3)
        XCTAssertEqual(output.state.scheduled[0].count, 480)
    }

    func testSlowDeviceHasBoundedQueueAndDropsOldAudio() async {
        let output = RecordingAudioOutput()
        let player = RemoteAudioPlayer(output: output)
        player.setSession(streamID: 42, enabled: true)
        for sequence in 1...4 { player.receive(streamID: 42, sequence: UInt64(sequence), pcm: silence) }
        await player.waitUntilIdleForTesting()
        for sequence in 5...200 { player.receive(streamID: 42, sequence: UInt64(sequence), pcm: silence) }
        await player.waitUntilIdleForTesting()
        XCTAssertLessThanOrEqual(output.state.scheduled.count, 4)
        XCTAssertLessThanOrEqual(player.audioQueueSnapshotForTesting.queuedFrames, 6)
        XCTAssertGreaterThan(player.audioQueueSnapshotForTesting.droppedFrames, 0)
        output.completeFirst()
        await player.waitUntilIdleForTesting()
        XCTAssertLessThanOrEqual(output.state.scheduled.count, 5)
    }

    func testStreamResetAndLateCompletionCannotReplayOldAudio() async {
        let output = RecordingAudioOutput()
        let player = RemoteAudioPlayer(output: output)
        player.setSession(streamID: 42, enabled: true)
        player.receive(streamID: 42, sequence: 1, pcm: silence)
        player.receive(streamID: 42, sequence: 2, pcm: silence)
        await player.waitUntilIdleForTesting()
        let oldCompletion = output.firstCompletion
        player.setSession(streamID: 43, enabled: true)
        player.receive(streamID: 42, sequence: 3, pcm: silence)
        oldCompletion?()
        player.receive(streamID: 43, sequence: 1, pcm: silence)
        player.receive(streamID: 43, sequence: 2, pcm: silence)
        await player.waitUntilIdleForTesting()
        XCTAssertEqual(output.state.starts, 2)
        XCTAssertEqual(output.state.scheduled.count, 4)
        player.stop()
        oldCompletion?()
        player.receive(streamID: 43, sequence: 3, pcm: silence)
        await player.waitUntilIdleForTesting()
        XCTAssertEqual(output.state.scheduled.count, 4)
        XCTAssertFalse(player.isPlaying)
        XCTAssertEqual(player.audioQueueSnapshotForTesting.queuedFrames, 0)
    }

    func testVolumeClampsAndMuteRetainsChosenVolume() async {
        let output = RecordingAudioOutput()
        let player = RemoteAudioPlayer(output: output)
        player.setVolume(1.5)
        player.setSession(streamID: 42, enabled: true)
        player.receive(streamID: 42, sequence: 1, pcm: silence)
        player.receive(streamID: 42, sequence: 2, pcm: silence)
        await player.waitUntilIdleForTesting()
        XCTAssertEqual(output.state.volume, 1)
        player.setMuted(true)
        player.setVolume(0.3)
        await player.waitUntilIdleForTesting()
        XCTAssertEqual(output.state.volume, 0)
        XCTAssertEqual(player.volume, 0.3)
        player.setMuted(false)
        player.setVolume(.nan)
        await player.waitUntilIdleForTesting()
        XCTAssertEqual(output.state.volume, 0.3, accuracy: 0.00001)
        player.setVolume(-1)
        await player.waitUntilIdleForTesting()
        XCTAssertEqual(output.state.volume, 0)
    }

    func testNativeOutputFailureStopsAndDoesNotConsumeFurtherPackets() async {
        let output = RecordingAudioOutput()
        output.failStart = true
        let player = RemoteAudioPlayer(output: output)
        player.setSession(streamID: 42, enabled: true)
        player.receive(streamID: 42, sequence: 1, pcm: silence)
        player.receive(streamID: 42, sequence: 2, pcm: silence)
        await player.waitUntilIdleForTesting()
        await Task.yield()
        player.receive(streamID: 42, sequence: 3, pcm: silence)
        player.receive(streamID: 42, sequence: 4, pcm: silence)
        await player.waitUntilIdleForTesting()
        XCTAssertEqual(output.state.starts, 1)
        XCTAssertEqual(output.state.scheduled.count, 0)
        XCTAssertNotNil(player.localError)
        XCTAssertFalse(player.isPlaying)
    }

    func testDeviceChangeDuringSchedulingStopsAndCanBeReopened() async {
        let output = RecordingAudioOutput()
        output.failSchedule = true
        let player = RemoteAudioPlayer(output: output)
        player.setSession(streamID: 42, enabled: true)
        player.receive(streamID: 42, sequence: 1, pcm: silence)
        player.receive(streamID: 42, sequence: 2, pcm: silence)
        await player.waitUntilIdleForTesting()
        await Task.yield()
        XCTAssertNotNil(player.localError)
        XCTAssertEqual(player.audioQueueSnapshotForTesting.queuedFrames, 0)
        output.failSchedule = false
        player.stop()
        player.setSession(streamID: 42, enabled: true)
        player.receive(streamID: 42, sequence: 1, pcm: silence)
        player.receive(streamID: 42, sequence: 2, pcm: silence)
        await player.waitUntilIdleForTesting()
        XCTAssertEqual(output.state.starts, 2)
        XCTAssertEqual(output.state.scheduled.count, 2)
        XCTAssertNil(player.localError)
    }

    /// Exercises the same PCM buffer conversion and native mixer graph without
    /// opening the system speaker. A nonzero rendered waveform proves output
    /// samples survive the AVAudioEngine path.
    func testSineWaveRendersThroughNativeEngineOffline() throws {
        let engine = AVAudioEngine()
        let node = AVAudioPlayerNode()
        let format = try XCTUnwrap(AVAudioFormat(standardFormatWithSampleRate: RemotePCM.sampleRate, channels: 1))
        engine.attach(node)
        engine.connect(node, to: engine.mainMixerNode, format: format)
        try engine.enableManualRenderingMode(.offline, format: format, maximumFrameCount: 480)
        let samples = (0..<480).map { Float(sin(2 * Double.pi * 440 * Double($0) / 48_000) * 0.2) }
        for _ in 0..<8 { node.scheduleBuffer(try NativeRemoteAudioOutput.buffer(samples: samples, format: format)) }
        try engine.start()
        node.play()
        defer { node.stop(); engine.stop() }
        let rendered = try XCTUnwrap(AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 480))
        var energy: Float = 0
        for _ in 0..<8 {
            let status = try engine.renderOffline(480, to: rendered)
            XCTAssertEqual(status, .success)
            let channel = try XCTUnwrap(rendered.floatChannelData?[0])
            for index in 0..<Int(rendered.frameLength) { energy += abs(channel[index]) }
        }
        XCTAssertGreaterThan(energy, 100)
    }

    private var silence: Data { Data(count: RemotePCM.bytesPerFrame) }
}

private final class RecordingAudioOutput: RemoteAudioOutput, @unchecked Sendable {
    struct State {
        var starts = 0
        var stops = 0
        var volume: Float = 0
        var scheduled: [[Float]] = []
    }
    private let lock = NSLock()
    private var storage = State()
    private var completions: [@Sendable () -> Void] = []
    var failStart = false
    var failSchedule = false
    var state: State { lock.withLock { storage } }
    var firstCompletion: (@Sendable () -> Void)? { lock.withLock { completions.first } }

    func start(volume: Float) throws {
        lock.withLock { storage.starts += 1; storage.volume = volume }
        if failStart { throw NSError(domain: "test", code: 1) }
    }
    func schedule(samples: [Float], completion: @escaping @Sendable () -> Void) throws {
        if failSchedule { throw NSError(domain: "test", code: 2) }
        lock.withLock { storage.scheduled.append(samples); completions.append(completion) }
    }
    func play() {}
    func setVolume(_ volume: Float) { lock.withLock { storage.volume = volume } }
    func stop() { lock.withLock { storage.stops += 1 } }
    func completeFirst() {
        let completion = lock.withLock { completions.isEmpty ? nil : completions.removeFirst() }
        completion?()
    }
}
