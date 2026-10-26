import Foundation
import XCTest
@testable import DeskLinkAppleCore

final class SavedHostStoreTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_000)
    private let directoryCode = Array("23456789".utf8)

    private func host(
        id: UUID = UUID(), key: UInt8 = 3, expiry: UInt64? = nil,
        authorization: UInt32? = nil, deviceID: UInt64? = nil,
        code: [UInt8]? = nil, alias: String? = nil, connectedAt: Date? = nil
    ) -> SavedHost {
        SavedHost(
            id: id, serverName: "relay.example.com",
            sessionID: [UInt8](repeating: key, count: 16),
            relayAuthentication: [UInt8](repeating: key, count: 32),
            hostVerifyKey: [UInt8](repeating: key, count: 32),
            expiresAtUnixS: expiry, authorizationType: authorization,
            directoryDeviceID: deviceID, directoryAccessCode: code,
            alias: alias, lastConnectedAt: connectedAt
        )
    }

    func testLegacyRecordRemainsReadableWithoutBecomingPermanent() throws {
        let legacy = host()
        let encoded = try SavedHostStore.encode([legacy])
        let decoded = try XCTUnwrap(SavedHostStore.decode(encoded).first)
        XCTAssertTrue(decoded.isValid)
        XCTAssertNil(decoded.expiresAtUnixS)
        XCTAssertNil(decoded.authorizationType)
        XCTAssertFalse(decoded.canReconnect(at: now))
    }

    func testTemporaryAuthorizationStopsExactlyAtExpiry() {
        let temporary = host(expiry: 1_001, authorization: 1)
        XCTAssertTrue(temporary.canReconnect(at: now))
        XCTAssertFalse(temporary.canReconnect(at: Date(timeIntervalSince1970: 1_001)))
        XCTAssertFalse(host(expiry: 0, authorization: 1).canReconnect(at: now))
        XCTAssertFalse(host(authorization: 1).canReconnect(at: now))
    }

    func testPersistentAuthorizationRequiresExplicitTypeAndZeroExpiry() {
        XCTAssertTrue(host(expiry: 0, authorization: 2).canReconnect(at: now))
        XCTAssertFalse(host(authorization: 2).canReconnect(at: now))
        XCTAssertFalse(host(expiry: 2_000, authorization: 2).canReconnect(at: now))
        XCTAssertFalse(host(expiry: 0, authorization: 0).canReconnect(at: now))
        XCTAssertFalse(host(expiry: 0).canReconnect(at: now))
        XCTAssertFalse(host(expiry: 0, authorization: 3).isValid)
    }

    func testDirectoryAuthorizationAndLocalMetadataRoundTripInKeychain() throws {
        let keychain = InMemoryKeychainStore()
        let store = SavedHostStore(keychain: keychain, service: "hosts", account: "saved")
        let saved = host(
            expiry: 0, authorization: 2, deviceID: 123_456_789_012,
            code: directoryCode, alias: "书房电脑", connectedAt: now
        )
        try store.save(saved)
        XCTAssertEqual(try store.loadAll(), [saved])
        XCTAssertEqual(String(describing: saved), "SavedHost([REDACTED])")
        XCTAssertEqual(String(reflecting: saved), "SavedHost([REDACTED])")
    }

    func testRefreshingHostRetainsRecordIdentityAndUserAlias() throws {
        let store = SavedHostStore(keychain: InMemoryKeychainStore())
        let previous = host(expiry: 1_001, authorization: 1, alias: "我的电脑", connectedAt: now)
        let refreshed = host(
            expiry: 0, authorization: 2, deviceID: 123_456_789_012,
            code: directoryCode, alias: "另一个名字", connectedAt: now.addingTimeInterval(5)
        )
        try store.save(previous)
        try store.save(refreshed)
        let merged = try XCTUnwrap(store.loadAll().first)
        XCTAssertEqual(merged.id, previous.id)
        XCTAssertEqual(merged.alias, previous.alias)
        XCTAssertEqual(merged.expiresAtUnixS, 0)
        XCTAssertEqual(merged.directoryAccessCode, directoryCode)
        XCTAssertEqual(merged.lastConnectedAt, refreshed.lastConnectedAt)
        XCTAssertEqual(try store.loadAll().count, 1)
    }

    func testRenamingPersistsAndClearingRestoresDefaultName() throws {
        let store = SavedHostStore(keychain: InMemoryKeychainStore())
        let saved = host(deviceID: 123_456_789_012, code: directoryCode)
        try store.save(saved)
        try store.rename(id: saved.id, alias: "  工作电脑  ")
        XCTAssertEqual(try store.loadAll().first?.displayName, "工作电脑")
        try store.rename(id: saved.id, alias: " ")
        XCTAssertEqual(try store.loadAll().first?.displayName, "设备 123456789012")
        XCTAssertThrowsError(try store.rename(id: saved.id, alias: "a\nb"))
        XCTAssertThrowsError(try store.rename(id: saved.id, alias: String(repeating: "a", count: 81)))
    }

    func testRecordUUIDCannotSilentlySwitchAuthenticatedIdentity() throws {
        let store = SavedHostStore(keychain: InMemoryKeychainStore())
        let saved = host()
        try store.save(saved)
        XCTAssertThrowsError(try store.save(host(id: saved.id, key: 4)))
        XCTAssertEqual(try store.loadAll(), [saved])
    }

    func testDisplayNameDistinguishesHostsOnOneRelayWithoutLeakingCredentials() {
        XCTAssertEqual(host(alias: "书房").displayName, "书房")
        XCTAssertEqual(host(deviceID: 123_456_789_012, code: directoryCode).displayName, "设备 123456789012")
        XCTAssertNotEqual(host(key: 3).displayName, host(key: 4).displayName)
        XCTAssertFalse(host().displayName.contains("relay.example.com"))
    }

    func testIncompleteOrInvalidDirectoryMaterialCannotReconnect() {
        XCTAssertFalse(host(expiry: 0, authorization: 2, deviceID: 1).isValid)
        XCTAssertFalse(host(expiry: 0, authorization: 2, code: directoryCode).isValid)
        XCTAssertFalse(host(expiry: 0, authorization: 2, deviceID: 0, code: directoryCode).isValid)
        XCTAssertFalse(host(expiry: 0, authorization: 2, deviceID: 1, code: [0, 0, 0, 0, 0, 0, 0, 0]).canReconnect(at: now))
    }
}
