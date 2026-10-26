import DeskLinkAppleCore
import Foundation
import Security

enum HostAccessCredentialsError: Error, Equatable, LocalizedError {
    case invalidDeviceID
    case invalidPassword
    case malformedRecord
    case keychain(OSStatus)
    case storageUnavailable
    case randomGeneration(OSStatus)

    var errorDescription: String? {
        switch self {
        case .invalidDeviceID:
            "本机设备身份无效，请重新启动 DeskLink。"
        case .invalidPassword:
            "访问密码应为 8 位字母或数字，不支持 0、1、I、O。"
        case .malformedRecord:
            "受保护的固定密码已损坏，请重新设置。"
        case .keychain, .storageUnavailable:
            "无法访问钥匙串中的固定密码，请完成系统授权后重试。"
        case .randomGeneration:
            "无法安全生成访问密码，请稍后重试。"
        }
    }
}

/// The public identifier is derived from the existing host identity. It is not a
/// newly generated credential and does not change when an access password changes.
enum HostPublicDeviceID {
    private static let base: UInt64 = 100_000_000_000
    private static let range: UInt64 = 900_000_000_000

    static func number(for identity: HostIdentity) throws -> UInt64 {
        guard identity.deviceID.count == HostIdentity.deviceIDLength else {
            throw HostAccessCredentialsError.invalidDeviceID
        }
        let prefix = identity.deviceID.prefix(8).reduce(UInt64(0)) { ($0 << 8) | UInt64($1) }
        return base + prefix % range
    }

    static func formatted(for identity: HostIdentity) throws -> String {
        let digits = String(try number(for: identity))
        return stride(from: 0, to: digits.count, by: 3).map { offset in
            let start = digits.index(digits.startIndex, offsetBy: offset)
            return String(digits[start..<digits.index(start, offsetBy: 3)])
        }.joined(separator: " ")
    }
}

/// Use `revealed` only at an explicit display/copy or runtime boundary. Normal
/// interpolation, reflection and test failures deliberately redact the value.
struct HostAccessPassword: Equatable, Sendable, CustomStringConvertible, CustomDebugStringConvertible,
    CustomReflectable
{
    static let length = 8
    private static let alphabet = Array("23456789ABCDEFGHJKLMNPQRSTUVWXYZ".utf8)
    let bytes: [UInt8]

    init(_ userInput: String) throws {
        let normalized = userInput.unicodeScalars.filter { $0 != " " && $0 != "-" }
        try self.init(bytes: Array(String(String.UnicodeScalarView(normalized)).uppercased().utf8))
    }

    /// Stored values and values received from the core must already be canonical.
    init(bytes: [UInt8]) throws {
        guard bytes.count == Self.length, bytes.allSatisfy(Self.alphabet.contains) else {
            throw HostAccessCredentialsError.invalidPassword
        }
        self.bytes = bytes
    }

    static func generate() throws -> HostAccessPassword {
        var random = [UInt8](repeating: 0, count: length)
        defer { _ = random.withUnsafeMutableBytes { $0.initializeMemory(as: UInt8.self, repeating: 0) } }
        let status = random.withUnsafeMutableBytes {
            SecRandomCopyBytes(kSecRandomDefault, Self.length, $0.baseAddress!)
        }
        guard status == errSecSuccess else { throw HostAccessCredentialsError.randomGeneration(status) }
        // There are exactly 32 allowed symbols, so masking has no modulo bias.
        return try HostAccessPassword(bytes: random.map { alphabet[Int($0 & 31)] })
    }

    var revealed: String { String(decoding: bytes, as: UTF8.self) }
    var description: String { "HostAccessPassword([REDACTED])" }
    var debugDescription: String { description }
    var customMirror: Mirror { Mirror(self, children: ["value": "[REDACTED]"]) }
}

protocol HostFixedAccessStoring: Sendable {
    func load() throws -> HostAccessPassword?
    func save(_ password: HostAccessPassword) throws
    func clear() throws
}

/// Fixed passwords are the only access passwords persisted here. The system
/// keychain performs an update/add, so a failed save never deletes the old value.
struct HostFixedAccessStore: HostFixedAccessStoring {
    static let service = "com.desklink.host-fixed-access"
    static let account = "primary-host"
    static let formatVersion: UInt8 = 1
    static let recordLength = 1 + HostAccessPassword.length

    private let keychain: any KeychainStore

    init(keychain: any KeychainStore = SystemKeychainStore()) {
        self.keychain = keychain
    }

    func load() throws -> HostAccessPassword? {
        let record: Data?
        do {
            record = try keychain.read(service: Self.service, account: Self.account)
        } catch {
            throw Self.storageError(error)
        }
        guard let record else { return nil }
        return try Self.decode(record)
    }

    func save(_ password: HostAccessPassword) throws {
        do {
            try keychain.write(Self.encode(password), service: Self.service, account: Self.account)
        } catch {
            throw Self.storageError(error)
        }
    }

    func clear() throws {
        do {
            try keychain.delete(service: Self.service, account: Self.account)
        } catch {
            throw Self.storageError(error)
        }
    }

    static func encode(_ password: HostAccessPassword) -> Data {
        Data([formatVersion] + password.bytes)
    }

    static func decode(_ record: Data) throws -> HostAccessPassword {
        guard record.count == recordLength, record.first == formatVersion else {
            throw HostAccessCredentialsError.malformedRecord
        }
        do {
            return try HostAccessPassword(bytes: Array(record.dropFirst()))
        } catch {
            throw HostAccessCredentialsError.malformedRecord
        }
    }

    private static func storageError(_ error: Error) -> HostAccessCredentialsError {
        if case let KeychainStoreError.status(status) = error { return .keychain(status) }
        return .storageUnavailable
    }
}
