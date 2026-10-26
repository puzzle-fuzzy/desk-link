import Foundation
import XCTest
@testable import DeskLinkAppleCore

final class KeychainStoreTests: XCTestCase {
    func testInMemoryStoreOverwritesAndDeletesValues() throws {
        let keychain = InMemoryKeychainStore()

        try keychain.write(Data("first".utf8), service: "service", account: "account")
        try keychain.write(Data("second".utf8), service: "service", account: "account")
        XCTAssertEqual(try keychain.read(service: "service", account: "account"), Data("second".utf8))

        try keychain.delete(service: "service", account: "account")
        XCTAssertNil(try keychain.read(service: "service", account: "account"))
    }

    func testIdentityStoreRejectsMalformedRecords() throws {
        let keychain = InMemoryKeychainStore()
        try keychain.write(Data([1, 2, 3]), service: "identity", account: "controller")
        let store = ControllerIdentityStore(
            keychain: keychain,
            service: "identity",
            account: "controller"
        )

        XCTAssertThrowsError(try store.loadOrCreate()) {
            XCTAssertEqual($0 as? ControllerIdentityStoreError, .malformedIdentity)
        }
    }

    func testSavedHostStoreRejectsMalformedRecordsAndRemovesLastHost() throws {
        let keychain = InMemoryKeychainStore()
        let store = SavedHostStore(keychain: keychain, service: "hosts", account: "saved")
        try keychain.write(Data("not-json".utf8), service: "hosts", account: "saved")
        XCTAssertThrowsError(try store.loadAll()) {
            XCTAssertEqual($0 as? SavedHostStoreError, .malformedRecord)
        }

        try keychain.delete(service: "hosts", account: "saved")
        let host = SavedHost(
            id: UUID(),
            serverName: "relay.example.com",
            sessionID: [UInt8](repeating: 1, count: 16),
            relayAuthentication: [UInt8](repeating: 2, count: 32),
            hostVerifyKey: [UInt8](repeating: 3, count: 32)
        )
        try store.save(host)
        XCTAssertEqual(try store.loadAll(), [host])
        try store.remove(id: host.id)
        XCTAssertTrue(try store.loadAll().isEmpty)
    }

    func testSavedHostStoreCanClearEveryHostForAccountLogout() throws {
        let keychain = InMemoryKeychainStore()
        let store = SavedHostStore(keychain: keychain, service: "hosts", account: "saved")
        let firstHost = SavedHost(
            id: UUID(),
            serverName: "relay-0.example.com",
            sessionID: [UInt8](repeating: 1, count: 16),
            relayAuthentication: [UInt8](repeating: 2, count: 32),
            hostVerifyKey: [UInt8](repeating: 3, count: 32)
        )
        let secondHost = SavedHost(
            id: UUID(),
            serverName: "relay-1.example.com",
            sessionID: [UInt8](repeating: 2, count: 16),
            relayAuthentication: [UInt8](repeating: 3, count: 32),
            hostVerifyKey: [UInt8](repeating: 4, count: 32)
        )
        let hosts = [firstHost, secondHost]
        for host in hosts { try store.save(host) }

        try store.removeAll()

        XCTAssertTrue(try store.loadAll().isEmpty)
    }

    func testSavingSameRemoteHostDeduplicatesAndMovesItToTheFront() throws {
        let keychain = InMemoryKeychainStore()
        let store = SavedHostStore(keychain: keychain, service: "hosts", account: "saved")
        let first = SavedHost(
            id: UUID(),
            serverName: "relay.example.com",
            sessionID: [UInt8](repeating: 1, count: 16),
            relayAuthentication: [UInt8](repeating: 2, count: 32),
            hostVerifyKey: [UInt8](repeating: 3, count: 32)
        )
        let second = SavedHost(
            id: UUID(),
            serverName: "relay-2.example.com",
            sessionID: [UInt8](repeating: 4, count: 16),
            relayAuthentication: [UInt8](repeating: 5, count: 32),
            hostVerifyKey: [UInt8](repeating: 6, count: 32)
        )
        let refreshedFirst = SavedHost(
            id: UUID(),
            serverName: first.serverName,
            sessionID: [UInt8](repeating: 8, count: 16),
            relayAuthentication: [UInt8](repeating: 7, count: 32),
            hostVerifyKey: first.hostVerifyKey
        )

        try store.save(first)
        try store.save(second)
        try store.save(refreshedFirst)

        let saved = try store.loadAll()
        XCTAssertEqual(saved.count, 2)
        XCTAssertEqual(saved[0].id, first.id)
        XCTAssertEqual(saved[0].sessionID, refreshedFirst.sessionID)
        XCTAssertEqual(saved[0].relayAuthentication, refreshedFirst.relayAuthentication)
        XCTAssertEqual(saved[0].hostVerifyKey, refreshedFirst.hostVerifyKey)
        XCTAssertEqual(saved[1], second)
    }
}

extension KeychainStoreTests {
    func testLegacySavedHostsDecodeWithoutPresentationMetadata() throws {
        let legacyHost = presentationHost()
        var records = try XCTUnwrap(JSONSerialization.jsonObject(with: SavedHostStore.encode([legacyHost])) as? [[String: Any]])
        records[0].removeValue(forKey: "deviceID")
        records[0].removeValue(forKey: "displayName")
        records[0].removeValue(forKey: "lastConnectedAt")

        let decoded = try SavedHostStore.decode(JSONSerialization.data(withJSONObject: records))

        XCTAssertEqual(decoded, [legacyHost])
        XCTAssertNil(decoded[0].deviceID)
        XCTAssertNil(decoded[0].displayName)
        XCTAssertNil(decoded[0].lastConnectedAt)
    }

    func testRenamingSavedHostPreservesCredentialsMetadataAndOrder() throws {
        let keychain = InMemoryKeychainStore()
        let store = SavedHostStore(keychain: keychain)
        let first = presentationHost(deviceID: "123456789012", lastConnectedAt: Date(timeIntervalSince1970: 100))
        let second = presentationHost(key: 4)
        try store.save(first)
        try store.save(second)

        try store.rename(id: first.id, displayName: "  家里的电脑  ")
        let hosts = try store.loadAll()

        XCTAssertEqual(hosts.map(\.id), [second.id, first.id])
        let renamed = try XCTUnwrap(hosts.last)
        XCTAssertEqual(renamed.displayName, "家里的电脑")
        XCTAssertEqual(renamed.deviceID, first.deviceID)
        XCTAssertEqual(renamed.lastConnectedAt, first.lastConnectedAt)
        XCTAssertEqual(renamed.sessionID, first.sessionID)
        XCTAssertEqual(renamed.relayAuthentication, first.relayAuthentication)
        XCTAssertEqual(renamed.hostVerifyKey, first.hostVerifyKey)
        XCTAssertEqual(renamed.serverName, first.serverName)

        try store.rename(id: first.id, displayName: "   ")
        XCTAssertNil(try store.loadAll().last?.displayName)
    }

    func testRefreshingSavedHostPreservesAliasAndDeviceIDAndUpdatesLastConnection() throws {
        let keychain = InMemoryKeychainStore()
        let store = SavedHostStore(keychain: keychain)
        let host = presentationHost(deviceID: "123456789012", displayName: "办公室", lastConnectedAt: Date(timeIntervalSince1970: 100))
        try store.save(host)
        let refreshed = SavedHost(
            id: UUID(),
            serverName: host.serverName,
            sessionID: [UInt8](repeating: 8, count: 16),
            relayAuthentication: [UInt8](repeating: 9, count: 32),
            hostVerifyKey: host.hostVerifyKey,
            lastConnectedAt: Date(timeIntervalSince1970: 200)
        )
        try store.save(refreshed)

        let saved = try XCTUnwrap(try store.loadAll().first)
        XCTAssertEqual(try store.loadAll().count, 1)
        // The UUID identifies the local record; a credential refresh for
        // the same authenticated host keeps the original stable id.
        XCTAssertEqual(saved.id, host.id)
        XCTAssertEqual(saved.displayName, "办公室")
        XCTAssertEqual(saved.deviceID, host.deviceID)
        XCTAssertEqual(saved.lastConnectedAt, refreshed.lastConnectedAt)
        XCTAssertEqual(saved.relayAuthentication, refreshed.relayAuthentication)
    }

    func testSavedHostRenameRejectsUnusableLabelsWithoutChangingRecord() throws {
        let keychain = InMemoryKeychainStore()
        let store = SavedHostStore(keychain: keychain)
        let host = presentationHost(displayName: "原名称")
        try store.save(host)

        XCTAssertThrowsError(try store.rename(id: host.id, displayName: String(repeating: "a", count: 81)))
        XCTAssertThrowsError(try store.rename(id: host.id, displayName: "bad\0name"))
        XCTAssertEqual(try store.loadAll(), [host])
    }

    private func presentationHost(
        key: UInt8 = 3,
        deviceID: String? = nil,
        displayName: String? = nil,
        lastConnectedAt: Date? = nil
    ) -> SavedHost {
        SavedHost(
            id: UUID(),
            serverName: "relay.example.com",
            sessionID: [UInt8](repeating: 1, count: 16),
            relayAuthentication: [UInt8](repeating: 2, count: 32),
            hostVerifyKey: [UInt8](repeating: key, count: 32),
            deviceID: deviceID,
            displayName: displayName,
            lastConnectedAt: lastConnectedAt
        )
    }
}
