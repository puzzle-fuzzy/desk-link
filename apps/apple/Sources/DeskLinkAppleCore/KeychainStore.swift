import Foundation
import Security

public enum KeychainStoreError: Error, Equatable, Sendable {
    case status(OSStatus)
}

extension KeychainStoreError {
    var status: OSStatus {
        if case let .status(status) = self { return status }
        return errSecIO
    }
}

public protocol KeychainStore: Sendable {
    func read(service: String, account: String) throws -> Data?
    func write(_ data: Data, service: String, account: String) throws
    func delete(service: String, account: String) throws
}

public struct SystemKeychainStore: KeychainStore, Sendable {
    public init() {}

    public func read(service: String, account: String) throws -> Data? {
        var result: CFTypeRef?
        let status = SecItemCopyMatching(
            query(service: service, account: account).merging([
                kSecReturnData as String: true,
                kSecMatchLimit as String: kSecMatchLimitOne,
            ]) { _, new in new } as CFDictionary,
            &result
        )
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else {
            throw KeychainStoreError.status(status)
        }
        return data
    }

    public func write(_ data: Data, service: String, account: String) throws {
        let base = query(service: service, account: account)
        let update = SecItemUpdate(base as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if update == errSecSuccess { return }
        guard update == errSecItemNotFound else { throw KeychainStoreError.status(update) }
        let add = SecItemAdd(base.merging([
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]) { _, new in new } as CFDictionary, nil)
        guard add == errSecSuccess else { throw KeychainStoreError.status(add) }
    }

    public func delete(service: String, account: String) throws {
        let status = SecItemDelete(query(service: service, account: account) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw KeychainStoreError.status(status)
        }
    }

    private func query(service: String, account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }
}

public final class InMemoryKeychainStore: KeychainStore, @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: Data] = [:]

    public init() {}

    public func read(service: String, account: String) throws -> Data? {
        lock.lock()
        defer { lock.unlock() }
        return values[key(service: service, account: account)]
    }

    public func write(_ data: Data, service: String, account: String) throws {
        lock.lock()
        values[key(service: service, account: account)] = data
        lock.unlock()
    }

    public func delete(service: String, account: String) throws {
        lock.lock()
        values.removeValue(forKey: key(service: service, account: account))
        lock.unlock()
    }

    private func key(service: String, account: String) -> String {
        "\(service)\u{0}\(account)"
    }
}
