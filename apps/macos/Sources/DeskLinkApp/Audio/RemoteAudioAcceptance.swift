#if DEBUG
import Foundation

/// A quiet half-second fixture for an explicit acceptance action. It shares
/// the remote PCM conversion/playback path and never records local audio.
enum RemoteAudioAcceptance {
    static let frameCount = 50

    static func sineFrame(sequence: UInt64) -> Data {
        var data = Data(capacity: RemotePCM.bytesPerFrame)
        for index in 0..<RemotePCM.samplesPerFrame {
            let position = Double(sequence * UInt64(RemotePCM.samplesPerFrame) + UInt64(index))
            let seconds = position / RemotePCM.sampleRate
            let fade = min(1, max(0, seconds / 0.02), max(0, (0.5 - seconds) / 0.02))
            let sample = Int16(sin(2 * Double.pi * 440 * seconds) * 0.08 * fade * 32_767)
            let bits = UInt16(bitPattern: sample)
            data.append(UInt8(bits & 0xff))
            data.append(UInt8(bits >> 8))
        }
        return data
    }
}
#endif
