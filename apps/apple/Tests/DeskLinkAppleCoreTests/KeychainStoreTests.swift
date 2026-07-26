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

        XCTAssertEqual(try store.loadAll(), [refreshedFirst, second])
    }
}
