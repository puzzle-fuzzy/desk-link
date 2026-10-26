import Foundation

public enum RemotePlatform: UInt8, Equatable, Sendable {
    case unknown = 0, windows = 1, macos = 2, ios = 3
}

/// FFI capability event v1, produced after authenticated negotiation.
public struct SessionCapabilities: Equatable, Sendable {
    public let platform: RemotePlatform
    public let width: UInt16
    public let height: UInt16
    public let h264Profiles: UInt8

    public init?(encoded: Data) {
        guard encoded.count == 7, encoded[0] == 1,
              let platform = RemotePlatform(rawValue: encoded[1]), platform != .unknown,
              encoded[6] & 1 != 0, encoded[6] & ~3 == 0 else { return nil }
        let width = UInt16(encoded[2]) | UInt16(encoded[3]) << 8
        let height = UInt16(encoded[4]) | UInt16(encoded[5]) << 8
        guard width > 0, height > 0 else { return nil }
        self.platform = platform
        self.width = width
        self.height = height
        self.h264Profiles = encoded[6]
    }
}
