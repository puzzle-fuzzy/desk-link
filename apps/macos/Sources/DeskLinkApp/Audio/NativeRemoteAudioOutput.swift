import AVFAudio
import Foundation

/// AVAudioEngine is confined to RemoteAudioScheduler's serial queue. It uses
/// only an output player node, so microphone permission is never requested.
final class NativeRemoteAudioOutput: RemoteAudioOutput, @unchecked Sendable {
    private let engine = AVAudioEngine()
    private let player = AVAudioPlayerNode()
    private let format = AVAudioFormat(standardFormatWithSampleRate: RemotePCM.sampleRate, channels: 1)!
    private var attached = false

    func start(volume: Float) throws {
        if !attached {
            engine.attach(player)
            engine.connect(player, to: engine.mainMixerNode, format: format)
            attached = true
        }
        player.volume = volume
        engine.prepare()
        try engine.start()
    }

    func schedule(samples: [Float], completion: @escaping @Sendable () -> Void) throws {
        guard engine.isRunning else { throw NativeAudioError.outputChanged }
        let buffer = try Self.buffer(samples: samples, format: format)
        player.scheduleBuffer(buffer, completionCallbackType: .dataPlayedBack) { _ in completion() }
    }

    func play() { if !player.isPlaying { player.play() } }
    func setVolume(_ volume: Float) { player.volume = volume }
    func stop() { player.stop(); engine.stop(); engine.reset() }

    static func buffer(samples: [Float], format: AVAudioFormat) throws -> AVAudioPCMBuffer {
        guard !samples.isEmpty, samples.count <= RemotePCM.samplesPerFrame,
              let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: AVAudioFrameCount(samples.count)),
              let destination = buffer.floatChannelData?[0] else { throw NativeAudioError.invalidBuffer }
        buffer.frameLength = AVAudioFrameCount(samples.count)
        samples.withUnsafeBufferPointer { source in
            if let baseAddress = source.baseAddress { destination.update(from: baseAddress, count: samples.count) }
        }
        return buffer
    }
}

private enum NativeAudioError: Error { case outputChanged, invalidBuffer }
