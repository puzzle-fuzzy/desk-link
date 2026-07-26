import DeskLinkC
import Foundation

public enum DeskLinkPlatform: Equatable, Sendable {
    case macos
    case ios

    var cValue: DesklinkPlatform {
        switch self {
        case .macos: DESKLINK_PLATFORM_MACOS
        case .ios: DESKLINK_PLATFORM_IOS
        }
    }
}

public struct DeskLinkRuntimeConfiguration: Equatable, Sendable {
    public let relayURL: String
    public let relayServerName: String
    public let platform: DeskLinkPlatform

    public init(relayURL: String, relayServerName: String, platform: DeskLinkPlatform) {
        self.relayURL = relayURL
        self.relayServerName = relayServerName
        self.platform = platform
    }

    public var isValid: Bool {
        !relayURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !relayServerName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !relayURL.utf8.contains(0)
            && !relayServerName.utf8.contains(0)
    }

    public static var macOSDefaults: Self {
        Self(
            relayURL: ProcessInfo.processInfo.environment["DESKLINK_RELAY_URL"] ?? "quic://127.0.0.1:4433",
            relayServerName: ProcessInfo.processInfo.environment["DESKLINK_RELAY_SERVER_NAME"] ?? "localhost",
            platform: .macos
        )
    }
}

public struct Modifiers: OptionSet, Equatable, Sendable {
    public let rawValue: UInt32

    public init(rawValue: UInt32) { self.rawValue = rawValue }

    public static let shift = Modifiers(rawValue: 1 << 0)
    public static let control = Modifiers(rawValue: 1 << 1)
    public static let option = Modifiers(rawValue: 1 << 2)
    public static let meta = Modifiers(rawValue: 1 << 3)
    public static let capsLock = Modifiers(rawValue: 1 << 4)
}

public enum MouseButton: UInt32, CaseIterable, Comparable, Sendable {
    case left = 0
    case right = 1
    case center = 2

    public static func < (lhs: MouseButton, rhs: MouseButton) -> Bool {
        lhs.rawValue < rhs.rawValue
    }
}

public enum RemoteInputCommand: Equatable, Sendable {
    case move(normalizedX: Float, normalizedY: Float)
    case mouseButton(MouseButton, pressed: Bool)
    case wheel(deltaX: Int32, deltaY: Int32)
    case key(code: UInt32, pressed: Bool, modifiers: Modifiers)
    case unicode(String, modifiers: Modifiers)
}

public enum ConnectionState: Equatable, Sendable {
    case idle
    case pairing
    case connecting
    case connected(streamID: UInt64)
    case reconnecting
    case recovering
    case frozen
    case closed
    case failed(String)
}

public enum AppRole: Hashable, Sendable {
    case controller
    case host
}

public enum HostState: Equatable, Sendable {
    case idle
    case connecting
    case waitingForApproval
    case negotiating
    case connected
    case stopping
    case closed
    case failed(String)
}

public struct HostPairingInvite: Equatable, Sendable {
    public let expiresAt: Date
    public let encoded: Data

    public init(expiresAt: Date, encoded: Data) {
        self.expiresAt = expiresAt
        self.encoded = encoded
    }
}

public struct HostApproval: Equatable, Identifiable, Sendable {
    public let id: UUID
    public let fingerprint: String
    public let controllerDeviceID: [UInt8]
    public let controllerVerifyKey: [UInt8]
    public var isApproved = false

    public init(
        id: UUID,
        fingerprint: String,
        controllerDeviceID: [UInt8],
        controllerVerifyKey: [UInt8],
        isApproved: Bool = false
    ) {
        self.id = id
        self.fingerprint = fingerprint
        self.controllerDeviceID = controllerDeviceID
        self.controllerVerifyKey = controllerVerifyKey
        self.isApproved = isApproved
    }

    public var deviceIDText: String {
        controllerDeviceID.map { String(format: "%02x", $0) }.joined(separator: ":")
    }
}

public struct HostMetrics: Equatable, Sendable {
    public var sentVideoConfigurations = 0
    public var sentVideoPackets = 0
    public var receivedInputEvents = 0
    public var keyframeRequests = 0

    public init() {}

    public init(
        sentVideoConfigurations: Int,
        sentVideoPackets: Int,
        receivedInputEvents: Int,
        keyframeRequests: Int
    ) {
        self.sentVideoConfigurations = sentVideoConfigurations
        self.sentVideoPackets = sentVideoPackets
        self.receivedInputEvents = receivedInputEvents
        self.keyframeRequests = keyframeRequests
    }
}

public struct PairingInfo: Equatable, Sendable {
    public let sessionID: UUID
    public let code: String
    public let expiresAt: Date

    public init(sessionID: UUID, code: String, expiresAt: Date) {
        self.sessionID = sessionID
        self.code = code
        self.expiresAt = expiresAt
    }
}

public struct Metrics: Equatable, Sendable {
    public var receivedFrames = 0
    public var droppedFrames = 0
    public var lastFrameID: UInt64?

    public init() {}
}

public struct CursorOverlay: Equatable, Sendable {
    public let streamID: UInt64
    public let encodedUpdate: Data

    public init(streamID: UInt64, encodedUpdate: Data) {
        self.streamID = streamID
        self.encodedUpdate = encodedUpdate
    }
}

public enum DeskLinkEvent {
    case state(ConnectionState)
    case error(String)
    case pairing(PairingInfo)
    case control(Data)
    case input(Data)
    case videoConfig(Data, width: UInt16, height: UInt16, version: UInt32)
    case h264(Data, streamID: UInt64, frameID: UInt64, configVersion: UInt32)
    case cursor(Data)
    case metrics(Data)
    case releaseAll
}
