import Foundation
import Security

struct SavedHost: Codable, Equatable, Identifiable, Sendable {
    let id: UUID
    let serverName: String
    let sessionID: [UInt8]
    let relayAuthentication: [UInt8]
    let hostVerifyKey: [UInt8]

    var isValid: Bool {
        !serverName.isEmpty && !serverName.utf8.contains(0)
            && sessionID.count == 16
            && relayAuthentication.count == 32
            && hostVerifyKey.count == 32
    }
}

enum SavedHostStoreError: Error, Equatable {
    case invalidRecord
    case malformedRecord
    case keychain(OSStatus)
}

struct SavedHostStore {
    private let service: String
    private let account: String

    init(service: String = "com.desklink.saved-hosts", account: String = "approved-controller-hosts") {
        self.service = service
        self.account = account
    }

    func save(_ host: SavedHost) throws {
        guard host.isValid else { throw SavedHostStoreError.invalidRecord }
        var hosts = try loadAll()
        if let index = hosts.firstIndex(where: { $0.id == host.id }) {
            hosts[index] = host
        } else {
            hosts.append(host)
        }
        try write(try Self.encode(hosts))
    }

    func loadAll() throws -> [SavedHost] {
        let query = baseQuery.merging([
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]) { _, new in new }
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return [] }
        guard status == errSecSuccess, let data = result as? Data else {
            throw SavedHostStoreError.keychain(status)
        }
        return try Self.decode(data)
    }

    func remove(id: UUID) throws {
        var hosts = try loadAll()
        hosts.removeAll { $0.id == id }
        if hosts.isEmpty {
            let status = SecItemDelete(baseQuery as CFDictionary)
            guard status == errSecSuccess || status == errSecItemNotFound else {
                throw SavedHostStoreError.keychain(status)
            }
        } else {
            try write(try Self.encode(hosts))
        }
    }

    static func encode(_ hosts: [SavedHost]) throws -> Data {
        guard hosts.allSatisfy(\.isValid) else { throw SavedHostStoreError.invalidRecord }
        return try JSONEncoder().encode(hosts)
    }

    static func decode(_ data: Data) throws -> [SavedHost] {
        guard let hosts = try? JSONDecoder().decode([SavedHost].self, from: data),
              hosts.allSatisfy(\.isValid)
        else { throw SavedHostStoreError.malformedRecord }
        return hosts
    }

    private var baseQuery: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    private func write(_ data: Data) throws {
        let update = SecItemUpdate(baseQuery as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if update == errSecSuccess { return }
        guard update == errSecItemNotFound else { throw SavedHostStoreError.keychain(update) }
        let add = SecItemAdd(baseQuery.merging([
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]) { _, new in new } as CFDictionary, nil)
        guard add == errSecSuccess else { throw SavedHostStoreError.keychain(add) }
    }
}
