import CryptoKit
import Foundation
import Security


public struct SavedHost: Codable, Equatable, Identifiable, Sendable, CustomStringConvertible, CustomDebugStringConvertible {
    private static let directoryAlphabet = Set("23456789ABCDEFGHJKLMNPQRSTUVWXYZ".utf8)
    public let id: UUID
    public let serverName: String
    public let sessionID: [UInt8]
    public let relayAuthentication: [UInt8]
    public let hostVerifyKey: [UInt8]
    public let expiresAtUnixS: UInt64?
    /// nil/0 is unknown legacy material, 1 temporary, 2 explicitly persistent.
    public let authorizationType: UInt32?
    public let directoryDeviceID: UInt64?
    public let directoryAccessCode: [UInt8]?
    public let alias: String?
    /// Optional metadata keeps older Keychain records decodable. Credentials
    /// remain in the existing protected fields and are never used as labels.
    public let deviceID: String?
    public let displayName: String?
    public let lastConnectedAt: Date?

    public init(
        id: UUID,
        serverName: String,
        sessionID: [UInt8],
        relayAuthentication: [UInt8],
        hostVerifyKey: [UInt8],
        expiresAtUnixS: UInt64? = nil,
        authorizationType: UInt32? = nil,
        directoryDeviceID: UInt64? = nil,
        directoryAccessCode: [UInt8]? = nil,
        alias: String? = nil,
        deviceID: String? = nil,
        displayName: String? = nil,
        lastConnectedAt: Date? = nil
    ) {
        self.id = id
        self.serverName = serverName
        self.sessionID = sessionID
        self.relayAuthentication = relayAuthentication
        self.hostVerifyKey = hostVerifyKey
        self.expiresAtUnixS = expiresAtUnixS
        self.authorizationType = authorizationType
        self.directoryDeviceID = directoryDeviceID
        self.directoryAccessCode = directoryAccessCode
        self.alias = Self.normalizedAlias(alias)
        self.deviceID = deviceID
        self.displayName = displayName
        self.lastConnectedAt = lastConnectedAt
    }

    public var isValid: Bool {
        !serverName.isEmpty && !serverName.utf8.contains(0)
            && sessionID.count == 16
            && relayAuthentication.count == 32
            && hostVerifyKey.count == 32
            && (authorizationType.map { $0 <= 2 } ?? true)
            && directoryIsStructurallyValid
            && Self.aliasIsValid(alias)
            && (lastConnectedAt?.timeIntervalSince1970.isFinite ?? true)
    }

    public var canReconnect: Bool { canReconnect(at: Date()) }

    public func canReconnect(at date: Date) -> Bool {
        guard isValid, date.timeIntervalSince1970.isFinite,
              date.timeIntervalSince1970 >= 0 else { return false }
        switch authorizationType {
        case 1:
            guard let expiresAtUnixS, expiresAtUnixS > 0 else { return false }
            return date.timeIntervalSince1970 < TimeInterval(expiresAtUnixS)
        case 2:
            return expiresAtUnixS == 0
        default:
            // Missing expiry/authorization in old records is never upgraded
            // into permanent access. The record stays available for re-pairing.
            return false
        }
    }

    /// Alias-first presentation fallback; the optional stored `displayName`
    /// carries an explicit user-facing label when present.
    public var displayTitle: String {
        if let label = displayName?.trimmingCharacters(in: .whitespacesAndNewlines), !label.isEmpty {
            return label
        }
        if let alias = Self.normalizedAlias(alias), Self.aliasIsValid(alias) { return alias }
        if let directoryDeviceID, directoryDeviceID > 0 { return "设备 \(directoryDeviceID)" }
        let fingerprint = SHA256.hash(data: Data(hostVerifyKey))
            .prefix(6).map { String(format: "%02x", $0) }.joined()
        return "设备 \(fingerprint)"
    }

    public var description: String { "SavedHost([REDACTED])" }
    public var debugDescription: String { description }

    private var directoryIsStructurallyValid: Bool {
        switch (directoryDeviceID, directoryAccessCode) {
        case (nil, nil): true
        case let (.some(deviceID), .some(code)):
            deviceID > 0 && code.count == 8
                && code.allSatisfy { Self.directoryAlphabet.contains($0) }
        default: false
        }
    }

    fileprivate static func normalizedAlias(_ alias: String?) -> String? {
        guard let value = alias?.trimmingCharacters(in: .whitespacesAndNewlines),
              !value.isEmpty else { return nil }
        return value
    }

    fileprivate static func aliasIsValid(_ alias: String?) -> Bool {
        guard let alias else { return true }
        return alias.count <= 80
            && !alias.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
    }

    fileprivate func withLocalMetadata(id: UUID, alias: String?, lastConnectedAt: Date?,
                                       deviceID: String?? = nil, displayName: String?? = nil) -> Self {
        Self(
            id: id, serverName: serverName, sessionID: sessionID,
            relayAuthentication: relayAuthentication, hostVerifyKey: hostVerifyKey,
            expiresAtUnixS: expiresAtUnixS, authorizationType: authorizationType,
            directoryDeviceID: directoryDeviceID, directoryAccessCode: directoryAccessCode,
            alias: alias, deviceID: deviceID ?? self.deviceID,
            displayName: displayName ?? self.displayName, lastConnectedAt: lastConnectedAt
        )
    }
}

public enum SavedHostStoreError: Error, Equatable, Sendable {
    case invalidRecord
    case malformedRecord
    case keychain(OSStatus)
}

public struct SavedHostStore: Sendable {
    private let keychain: any KeychainStore
    private let service: String
    private let account: String

    public init(
        keychain: any KeychainStore = SystemKeychainStore(),
        service: String = "com.desklink.saved-hosts",
        account: String = "approved-controller-hosts"
    ) {
        self.keychain = keychain
        self.service = service
        self.account = account
    }

    public func save(_ host: SavedHost) throws {
        guard host.isValid else { throw SavedHostStoreError.invalidRecord }
        var hosts = try loadAll()
        guard !hosts.contains(where: { $0.id == host.id && !Self.sameRemoteHost($0, host) })
        else { throw SavedHostStoreError.invalidRecord }
        // The UUID identifies a local record, not the remote host. Directory
        // lookup and fresh pairing can produce a new local UUID for the same
        // host, so de-duplicate by authenticated host identity and move the
        // successful connection to the front.
        let previous = hosts.first { $0.id == host.id || Self.sameRemoteHost($0, host) }
        let mergedDeviceID: String? = host.deviceID ?? previous?.deviceID
        let mergedDisplayName: String? = host.displayName ?? previous?.displayName
        let merged = host.withLocalMetadata(
            id: previous?.id ?? host.id,
            alias: previous?.alias ?? host.alias,
            lastConnectedAt: host.lastConnectedAt ?? previous?.lastConnectedAt,
            deviceID: mergedDeviceID,
            displayName: mergedDisplayName
        )
        hosts.removeAll { existing in
            existing.id == host.id || Self.sameRemoteHost(existing, host)
        }
        hosts.insert(merged, at: 0)
        try write(try Self.encode(hosts))
    }

    public func rename(id: UUID, alias: String?) throws {
        let normalized = SavedHost.normalizedAlias(alias)
        guard SavedHost.aliasIsValid(normalized) else { throw SavedHostStoreError.invalidRecord }
        var hosts = try loadAll()
        guard let index = hosts.firstIndex(where: { $0.id == id }) else {
            throw SavedHostStoreError.invalidRecord
        }
        let host = hosts[index]
        hosts[index] = host.withLocalMetadata(
            id: id,
            alias: normalized,
            lastConnectedAt: host.lastConnectedAt,
            deviceID: host.deviceID,
            displayName: host.displayName
        )
        try write(try Self.encode(hosts))
    }

    public func loadAll() throws -> [SavedHost] {
        do {
            guard let data = try keychain.read(service: service, account: account) else { return [] }
            return try Self.decode(data)
        } catch let error as KeychainStoreError {
            throw SavedHostStoreError.keychain(error.status)
        }
    }

    public func remove(id: UUID) throws {
        var hosts = try loadAll()
        hosts.removeAll { $0.id == id }
        if hosts.isEmpty {
            do {
                try keychain.delete(service: service, account: account)
            } catch let error as KeychainStoreError {
                throw SavedHostStoreError.keychain(error.status)
            }
        } else {
            try write(try Self.encode(hosts))
        }
    }

    /// Renaming only changes presentation metadata; it cannot replace approved
    /// identity, credentials, device ID, or the connection order. Empty clears
    /// the alias and restores the device ID / identity fallback.
    public func rename(id: UUID, displayName: String) throws {
        var hosts = try loadAll()
        guard let index = hosts.firstIndex(where: { $0.id == id }) else { return }
        let trimmed = displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count <= 80, !trimmed.unicodeScalars.contains(where: {
            CharacterSet.controlCharacters.contains($0)
        }) else { throw SavedHostStoreError.invalidRecord }
        let host = hosts[index]
        hosts[index] = SavedHost(
            id: host.id,
            serverName: host.serverName,
            sessionID: host.sessionID,
            relayAuthentication: host.relayAuthentication,
            hostVerifyKey: host.hostVerifyKey,
            deviceID: host.deviceID,
            displayName: trimmed.isEmpty ? nil : trimmed,
            lastConnectedAt: host.lastConnectedAt
        )
        try write(try Self.encode(hosts))
    }

    public func removeAll() throws {
        do {
            try keychain.delete(service: service, account: account)
        } catch let error as KeychainStoreError {
            throw SavedHostStoreError.keychain(error.status)
        }
    }

    public static func encode(_ hosts: [SavedHost]) throws -> Data {
        guard hosts.allSatisfy(\.isValid) else { throw SavedHostStoreError.invalidRecord }
        return try JSONEncoder().encode(hosts)
    }

    public static func decode(_ data: Data) throws -> [SavedHost] {
        guard let hosts = try? JSONDecoder().decode([SavedHost].self, from: data),
              hosts.allSatisfy(\.isValid)
        else { throw SavedHostStoreError.malformedRecord }
        return hosts
    }

    private func write(_ data: Data) throws {
        do {
            try keychain.write(data, service: service, account: account)
        } catch let error as KeychainStoreError {
            throw SavedHostStoreError.keychain(error.status)
        }
    }

    private static func sameRemoteHost(_ lhs: SavedHost, _ rhs: SavedHost) -> Bool {
        lhs.serverName == rhs.serverName
            && lhs.hostVerifyKey == rhs.hostVerifyKey
    }
}
