import Foundation

enum ConnectionState: Equatable {
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

enum AppRole: Hashable {
    case controller
    case host
}

enum HostState: Equatable {
    case idle
    case connecting
    case waitingForApproval
    case negotiating
    case connected
    case stopping
    case closed
    case failed(String)
}

struct HostPairingInvite: Equatable {
    let expiresAt: Date
    let encoded: Data
}

struct HostApproval: Equatable, Identifiable {
    let id: UUID
    let fingerprint: String
    let controllerDeviceID: [UInt8]
    let controllerVerifyKey: [UInt8]
    var isApproved = false

    var deviceIDText: String {
        controllerDeviceID.map { String(format: "%02x", $0) }.joined(separator: ":")
    }
}

struct HostMetrics: Equatable {
    var sentVideoConfigurations = 0
    var sentVideoPackets = 0
    var receivedInputEvents = 0
    var keyframeRequests = 0
}

struct PairingInfo: Equatable {
    let sessionID: UUID
    let code: String
    let expiresAt: Date
}

struct Metrics: Equatable {
    var receivedFrames = 0
    var droppedFrames = 0
    var lastFrameID: UInt64?
}

enum DeskLinkEvent {
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
