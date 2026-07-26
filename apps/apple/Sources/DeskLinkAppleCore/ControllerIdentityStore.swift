import DeskLinkC
import Foundation
import Security

public struct ControllerIdentity: Equatable, Sendable {
    public let deviceID: [UInt8]
    public let secretKey: [UInt8]
    public let verifyKey: [UInt8]

    public init(deviceID: [UInt8], secretKey: [UInt8], verifyKey: [UInt8]) {
        self.deviceID = deviceID
        self.secretKey = secretKey
        self.verifyKey = verifyKey
    }
}

public enum ControllerIdentityStoreError: LocalizedError, Equatable, Sendable {
    case keychain(OSStatus)
    case malformedIdentity
    case randomGeneration(OSStatus)
    case verifyKey(Int32)

    public var errorDescription: String? {
        switch self {
        case let .keychain(status): "Keychain operation failed (OSStatus \(status))."
        case .malformedIdentity: "The stored DeskLink identity is malformed."
        case let .randomGeneration(status): "Secure identity generation failed (OSStatus \(status))."
        case let .verifyKey(code): "Ed25519 public-key derivation failed (code \(code))."
        }
    }
}

public struct ControllerIdentityStore: Sendable {
    private let keychain: any KeychainStore
    private let service: String
    private let account: String

    public init(
        keychain: any KeychainStore = SystemKeychainStore(),
        service: String = "com.desklink.device-identity",
        account: String = "primary-controller"
    ) {
        self.keychain = keychain
        self.service = service
        self.account = account
    }

    public func loadOrCreate() throws -> ControllerIdentity {
        if let data = try load() {
            return try decode(data)
        }
        var bytes = [UInt8](repeating: 0, count: 48)
        let status = bytes.withUnsafeMutableBytes { buffer in
            SecRandomCopyBytes(kSecRandomDefault, buffer.count, buffer.baseAddress!)
        }
        guard status == errSecSuccess else {
            throw ControllerIdentityStoreError.randomGeneration(status)
        }
        let data = Data(bytes)
        do {
            try keychain.write(data, service: service, account: account)
        } catch let error as KeychainStoreError {
            throw ControllerIdentityStoreError.keychain(error.status)
        }
        return try decode(data)
    }

    private func load() throws -> Data? {
        do {
            return try keychain.read(service: service, account: account)
        } catch let error as KeychainStoreError {
            throw ControllerIdentityStoreError.keychain(error.status)
        }
    }

    private func decode(_ data: Data) throws -> ControllerIdentity {
        guard data.count == 48 else { throw ControllerIdentityStoreError.malformedIdentity }
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
        return ControllerIdentity(deviceID: deviceID, secretKey: secretKey, verifyKey: verifyKey)
    }
}
