import DeskLinkC
import Foundation
import Security

struct ControllerIdentity: Equatable {
    let deviceID: [UInt8]
    let secretKey: [UInt8]
    let verifyKey: [UInt8]
}

enum ControllerIdentityStoreError: LocalizedError {
    case keychain(OSStatus)
    case malformedIdentity
    case randomGeneration(OSStatus)
    case verifyKey(Int32)

    var errorDescription: String? {
        switch self {
        case let .keychain(status): "Keychain operation failed (OSStatus \(status))."
        case .malformedIdentity: "The stored DeskLink identity is malformed."
        case let .randomGeneration(status):
            "Secure identity generation failed (OSStatus \(status))."
        case let .verifyKey(code): "Ed25519 public-key derivation failed (code \(code))."
        }
    }
}

struct ControllerIdentityStore {
    private let service = "com.desklink.device-identity"
    private let account = "primary-controller"

    func loadOrCreate() throws -> ControllerIdentity {
        if let data = try load() {
            return try decode(data)
        }
        var bytes = [UInt8](repeating: 0, count: 48)
        let byteCount = bytes.count
        let status = bytes.withUnsafeMutableBytes { buffer in
            SecRandomCopyBytes(kSecRandomDefault, byteCount, buffer.baseAddress!)
        }
        guard status == errSecSuccess else {
            throw ControllerIdentityStoreError.randomGeneration(status)
        }
        let data = Data(bytes)
        let addStatus = SecItemAdd(
            baseQuery.merging([
                kSecValueData as String: data,
                kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            ]) { _, new in new } as CFDictionary,
            nil
        )
        if addStatus == errSecDuplicateItem, let existing = try load() {
            return try decode(existing)
        }
        guard addStatus == errSecSuccess else {
            throw ControllerIdentityStoreError.keychain(addStatus)
        }
        return try decode(data)
    }

    private var baseQuery: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    private func load() throws -> Data? {
        let query = baseQuery.merging([
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]) { _, new in new }
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else {
            throw ControllerIdentityStoreError.keychain(status)
        }
        return data
    }

    private func decode(_ data: Data) throws -> ControllerIdentity {
        guard data.count == 48 else {
            throw ControllerIdentityStoreError.malformedIdentity
        }
        let bytes = [UInt8](data)
        let deviceID = Array(bytes[..<16])
        let secretKey = Array(bytes[16...])
        var verifyKey = [UInt8](repeating: 0, count: 32)
        let result = secretKey.withUnsafeBufferPointer { secret in
            verifyKey.withUnsafeMutableBufferPointer { output in
                desklink_identity_verify_key(secret.baseAddress, output.baseAddress)
            }
        }
        guard result == DESKLINK_OK else {
            throw ControllerIdentityStoreError.verifyKey(Int32(result.rawValue))
        }
        return ControllerIdentity(
            deviceID: deviceID,
            secretKey: secretKey,
            verifyKey: verifyKey
        )
    }
}
