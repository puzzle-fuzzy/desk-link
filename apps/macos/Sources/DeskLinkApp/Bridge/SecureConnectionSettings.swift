import DeskLinkC
import Foundation

enum SecureConnectionSettingsError: LocalizedError, Equatable {
    case missing(String)
    case invalidHex(String, expectedBytes: Int)
    case invalidSavedHost
    case rejectedPairingInvite(Int32)

    var errorDescription: String? {
        switch self {
        case let .missing(name):
            "Missing required environment variable \(name)."
        case let .invalidHex(name, expectedBytes):
            "\(name) must contain exactly \(expectedBytes * 2) hexadecimal characters."
        case .invalidSavedHost:
            "The saved host record is invalid."
        case let .rejectedPairingInvite(code):
            "The signed pairing invitation was rejected (code \(code))."
        }
    }
}

struct SecureConnectionSettings: Equatable {
    let serverName: String
    let sessionID: [UInt8]
    let relayAuthentication: [UInt8]
    let hostVerifyKey: [UInt8]

    init(savedHost: SavedHost) throws {
        guard savedHost.isValid else { throw SecureConnectionSettingsError.invalidSavedHost }
        serverName = savedHost.serverName
        sessionID = savedHost.sessionID
        relayAuthentication = savedHost.relayAuthentication
        hostVerifyKey = savedHost.hostVerifyKey
    }

    var savedHost: SavedHost {
        SavedHost(
            id: UUID(),
            serverName: serverName,
            sessionID: sessionID,
            relayAuthentication: relayAuthentication,
            hostVerifyKey: hostVerifyKey
        )
    }

    init(environment: [String: String] = ProcessInfo.processInfo.environment) throws {
        serverName = environment["DESKLINK_RELAY_SERVER_NAME"] ?? "localhost"
        guard !serverName.isEmpty else {
            throw SecureConnectionSettingsError.missing("DESKLINK_RELAY_SERVER_NAME")
        }
        sessionID = try Self.decode(
            environment["DESKLINK_SESSION_ID"],
            name: "DESKLINK_SESSION_ID",
            count: 16
        )
        relayAuthentication = try Self.decode(
            environment["DESKLINK_AUTH_KEY"],
            name: "DESKLINK_AUTH_KEY",
            count: 32
        )
        hostVerifyKey = try Self.decode(
            environment["DESKLINK_HOST_VERIFY_KEY"],
            name: "DESKLINK_HOST_VERIFY_KEY",
            count: 32
        )
    }

    static func decode(
        _ value: String?,
        name: String,
        count: Int
    ) throws -> [UInt8] {
        guard let value else { throw SecureConnectionSettingsError.missing(name) }
        guard value.utf8.count == count * 2 else {
            throw SecureConnectionSettingsError.invalidHex(name, expectedBytes: count)
        }
        var bytes: [UInt8] = []
        bytes.reserveCapacity(count)
        var index = value.startIndex
        for _ in 0..<count {
            let next = value.index(index, offsetBy: 2)
            guard let byte = UInt8(value[index..<next], radix: 16) else {
                throw SecureConnectionSettingsError.invalidHex(name, expectedBytes: count)
            }
            bytes.append(byte)
            index = next
        }
        return bytes
    }
}

struct PairingInviteConnectionSettings: Equatable {
    let serverName: String
    let invite: [UInt8]

    init(environment: [String: String] = ProcessInfo.processInfo.environment) throws {
        serverName = environment["DESKLINK_RELAY_SERVER_NAME"] ?? "localhost"
        guard !serverName.isEmpty else {
            throw SecureConnectionSettingsError.missing("DESKLINK_RELAY_SERVER_NAME")
        }
        invite = try SecureConnectionSettings.decode(
            environment["DESKLINK_PAIRING_INVITE"],
            name: "DESKLINK_PAIRING_INVITE",
            count: Int(DESKLINK_PAIRING_INVITE_BYTES)
        )
    }
}
