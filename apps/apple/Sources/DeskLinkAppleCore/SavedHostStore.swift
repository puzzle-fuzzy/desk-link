import Foundation
import Security

public struct SavedHost: Codable, Equatable, Identifiable, Sendable {
    public let id: UUID
    public let serverName: String
    public let sessionID: [UInt8]
    public let relayAuthentication: [UInt8]
    public let hostVerifyKey: [UInt8]

    public init(
        id: UUID,
        serverName: String,
        sessionID: [UInt8],
        relayAuthentication: [UInt8],
        hostVerifyKey: [UInt8]
    ) {
        self.id = id
        self.serverName = serverName
        self.sessionID = sessionID
        self.relayAuthentication = relayAuthentication
        self.hostVerifyKey = hostVerifyKey
    }

    public var isValid: Bool {
        !serverName.isEmpty && !serverName.utf8.contains(0)
            && sessionID.count == 16
            && relayAuthentication.count == 32
            && hostVerifyKey.count == 32
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
        if let index = hosts.firstIndex(where: { $0.id == host.id }) {
            hosts[index] = host
        } else {
            hosts.append(host)
        }
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
}
