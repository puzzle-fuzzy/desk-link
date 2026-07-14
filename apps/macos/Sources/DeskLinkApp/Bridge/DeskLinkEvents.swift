import Foundation

enum ConnectionState: Equatable {
    case idle
    case pairing
    case connecting
    case connected(streamID: UInt64)
    case closed
    case failed(String)
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
