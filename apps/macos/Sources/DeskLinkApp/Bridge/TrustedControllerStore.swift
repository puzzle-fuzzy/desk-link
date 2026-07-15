import Foundation
import Security

struct TrustedController: Equatable, Sendable {
    static let deviceIDLength = 16
    static let verifyKeyLength = 32

    let deviceID: [UInt8]
    let verifyKey: [UInt8]
    let approvedAtUnixSeconds: UInt64
    let lastSeenAtUnixSeconds: UInt64
    let displayName: String

    init(
        deviceID: [UInt8],
        verifyKey: [UInt8],
        approvedAtUnixSeconds: UInt64,
        lastSeenAtUnixSeconds: UInt64? = nil,
        displayName: String
    ) {
        self.deviceID = deviceID
        self.verifyKey = verifyKey
        self.approvedAtUnixSeconds = approvedAtUnixSeconds
        self.lastSeenAtUnixSeconds = lastSeenAtUnixSeconds ?? approvedAtUnixSeconds
        self.displayName = displayName
    }
}

enum TrustedControllerStoreError: Error, Equatable, LocalizedError {
    case malformedRecord
    case duplicateDeviceID
    case duplicateRecord
    case capacityReached
    case invalidRecord
    case keychain(OSStatus)

    var errorDescription: String? {
        switch self {
        case .malformedRecord: "The stored trusted-controller data is malformed."
        case .duplicateDeviceID: "This device ID is already trusted with a different key."
        case .duplicateRecord: "This controller is already trusted."
        case .capacityReached: "The trusted-controller capacity has been reached."
        case .invalidRecord: "The trusted-controller record is invalid."
        case let .keychain(status): "Keychain operation failed (OSStatus \(status))."
        }
    }
}

struct TrustedControllerStore {
    static let formatVersion: UInt8 = 1
    static let maximumRecords = 64
    private static let displayNameBytes = 63
    private static let recordLength = 16 + 32 + 8 + 8 + 1 + displayNameBytes
    private static let headerLength = 3

    private let service = "com.desklink.trusted-controllers"
    private let account = "primary-host"

    func list() throws -> [TrustedController] {
        guard let data = try loadData() else { return [] }
        return try Self.decode(data)
    }

    func save(_ records: [TrustedController]) throws {
        let data = try Self.encode(records)
        let status = SecItemUpdate(query as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecItemNotFound {
            let addStatus = SecItemAdd(
                query.merging([
                    kSecValueData as String: data,
                    kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
                ]) { _, new in new } as CFDictionary,
                nil
            )
            guard addStatus == errSecSuccess else { throw TrustedControllerStoreError.keychain(addStatus) }
            return
        }
        guard status == errSecSuccess else { throw TrustedControllerStoreError.keychain(status) }
    }

    func trust(_ controller: TrustedController) throws {
        var records = try list()
        if let existing = records.first(where: { $0.deviceID == controller.deviceID }) {
            if existing.verifyKey == controller.verifyKey { throw TrustedControllerStoreError.duplicateRecord }
            throw TrustedControllerStoreError.duplicateDeviceID
        }
        records.append(controller)
        try save(records)
    }

    @discardableResult
    func revoke(deviceID: [UInt8]) throws -> Bool {
        var records = try list()
        let originalCount = records.count
        records.removeAll { $0.deviceID == deviceID }
        guard records.count != originalCount else { return false }
        try save(records)
        return true
    }

    static func encode(_ records: [TrustedController]) throws -> Data {
        guard records.count <= maximumRecords else { throw TrustedControllerStoreError.capacityReached }
        var seenIDs = Set<[UInt8]>()
        var seenRecords = Set<Data>()
        var output = Data([formatVersion, UInt8(records.count >> 8), UInt8(records.count)])
        output.reserveCapacity(headerLength + records.count * recordLength)
        for record in records {
            guard record.deviceID.count == TrustedController.deviceIDLength,
                  record.verifyKey.count == TrustedController.verifyKeyLength,
                  let name = record.displayName.data(using: .utf8), name.count <= displayNameBytes
            else { throw TrustedControllerStoreError.invalidRecord }
            let fingerprint = Data(record.deviceID + record.verifyKey)
            guard seenRecords.insert(fingerprint).inserted else {
                throw TrustedControllerStoreError.duplicateRecord
            }
            guard seenIDs.insert(record.deviceID).inserted else {
                throw TrustedControllerStoreError.duplicateDeviceID
            }
            output.append(contentsOf: record.deviceID)
            output.append(contentsOf: record.verifyKey)
            output.append(contentsOf: withUnsafeBytes(of: record.approvedAtUnixSeconds.bigEndian, Array.init))
            output.append(contentsOf: withUnsafeBytes(of: record.lastSeenAtUnixSeconds.bigEndian, Array.init))
            output.append(UInt8(name.count))
            output.append(name)
            output.append(Data(repeating: 0, count: displayNameBytes - name.count))
        }
        return output
    }

    static func decode(_ data: Data) throws -> [TrustedController] {
        let bytes = [UInt8](data)
        guard bytes.count >= headerLength, bytes[0] == formatVersion else {
            throw TrustedControllerStoreError.malformedRecord
        }
        let count = Int(bytes[1]) << 8 | Int(bytes[2])
        guard count <= maximumRecords, bytes.count == headerLength + count * recordLength else {
            throw TrustedControllerStoreError.malformedRecord
        }
        var records: [TrustedController] = []
        var offset = headerLength
        for _ in 0..<count {
            let deviceID = Array(bytes[offset..<(offset + 16)])
            offset += 16
            let verifyKey = Array(bytes[offset..<(offset + 32)])
            offset += 32
            let approvedAt = bytes[offset..<(offset + 8)].reduce(UInt64(0)) { $0 << 8 | UInt64($1) }
            offset += 8
            let lastSeenAt = bytes[offset..<(offset + 8)].reduce(UInt64(0)) { $0 << 8 | UInt64($1) }
            offset += 8
            let nameLength = Int(bytes[offset])
            offset += 1
            let nameBytes = bytes[offset..<(offset + displayNameBytes)]
            guard nameLength <= displayNameBytes,
                  nameBytes.dropFirst(nameLength).allSatisfy({ $0 == 0 }),
                  let name = String(bytes: nameBytes.prefix(nameLength), encoding: .utf8)
            else { throw TrustedControllerStoreError.malformedRecord }
            offset += displayNameBytes
            records.append(TrustedController(
                deviceID: deviceID,
                verifyKey: verifyKey,
                approvedAtUnixSeconds: approvedAt,
                lastSeenAtUnixSeconds: lastSeenAt,
                displayName: name
            ))
        }
        _ = try encode(records)
        return records
    }

    private var query: [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    private func loadData() throws -> Data? {
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
            throw TrustedControllerStoreError.keychain(status)
        }
        return data
    }
}
