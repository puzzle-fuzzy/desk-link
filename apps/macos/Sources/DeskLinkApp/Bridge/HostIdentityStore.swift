import Foundation
import Security

struct HostIdentity: Equatable, Sendable {
    static let deviceIDLength = 16
    static let secretKeyLength = 32

    let deviceID: [UInt8]
    let secretKey: [UInt8]
}

enum HostIdentityStoreError: Error, Equatable, LocalizedError {
    case invalidIdentityLength
    case malformedRecord
    case keychain(OSStatus)
    case randomGeneration(OSStatus)

    var errorDescription: String? {
        switch self {
        case .invalidIdentityLength: "The DeskLink host identity has an invalid length."
        case .malformedRecord: "The stored DeskLink host identity is malformed."
        case let .keychain(status): "Keychain operation failed (OSStatus \(status))."
        case let .randomGeneration(status): "Secure identity generation failed (OSStatus \(status))."
        }
    }
}

struct HostIdentityStore {
    static let formatVersion: UInt8 = 1
    static let recordLength = 1 + HostIdentity.deviceIDLength + HostIdentity.secretKeyLength

    private let service = "com.desklink.host-identity"
    private let account = "primary-host"

    func loadOrCreate() throws -> HostIdentity {
        if let identity = try load() { return identity }
        let identity = try makeIdentity()
        let status = SecItemAdd(
            query.merging([
                kSecValueData as String: try Self.encode(identity),
                kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            ]) { _, new in new } as CFDictionary,
            nil
        )
        if status == errSecDuplicateItem, let existing = try load() { return existing }
        guard status == errSecSuccess else { throw HostIdentityStoreError.keychain(status) }
        return identity
    }

    func load() throws -> HostIdentity? {
        var result: CFTypeRef?
        let status = SecItemCopyMatching(
            query.merging([
                kSecReturnData as String: true,
                kSecMatchLimit as String: kSecMatchLimitOne,
            ]) { _, new in new } as CFDictionary,
            &result
        )
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else {
            throw HostIdentityStoreError.keychain(status)
        }
        return try Self.decode(data)
    }

    func save(_ identity: HostIdentity) throws {
        let data = try Self.encode(identity)
        let status = SecItemUpdate(query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecItemNotFound {
            let addStatus = SecItemAdd(
                query.merging([
                    kSecValueData as String: data,
                    kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
                ]) { _, new in new } as CFDictionary,
                nil
            )
            guard addStatus == errSecSuccess else { throw HostIdentityStoreError.keychain(addStatus) }
            return
        }
        guard status == errSecSuccess else { throw HostIdentityStoreError.keychain(status) }
    }

    static func encode(_ identity: HostIdentity) throws -> Data {
        guard identity.deviceID.count == HostIdentity.deviceIDLength,
              identity.secretKey.count == HostIdentity.secretKeyLength
        else { throw HostIdentityStoreError.invalidIdentityLength }
        return Data([formatVersion] + identity.deviceID + identity.secretKey)
    }

    static func decode(_ data: Data) throws -> HostIdentity {
        guard data.count == recordLength, data.first == formatVersion else {
            throw HostIdentityStoreError.malformedRecord
        }
        let bytes = [UInt8](data)
        return HostIdentity(
            deviceID: Array(bytes[1..<(1 + HostIdentity.deviceIDLength)]),
            secretKey: Array(bytes[(1 + HostIdentity.deviceIDLength)...])
        )
    }

    private var query: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    private func makeIdentity() throws -> HostIdentity {
        var bytes = [UInt8](repeating: 0, count: HostIdentity.deviceIDLength + HostIdentity.secretKeyLength)
        let byteCount = bytes.count
        let status = bytes.withUnsafeMutableBytes {
            SecRandomCopyBytes(kSecRandomDefault, byteCount, $0.baseAddress!)
        }
        guard status == errSecSuccess else { throw HostIdentityStoreError.randomGeneration(status) }
        return HostIdentity(
            deviceID: Array(bytes[..<HostIdentity.deviceIDLength]),
            secretKey: Array(bytes[HostIdentity.deviceIDLength...])
        )
    }
}
