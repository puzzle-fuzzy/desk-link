import DeskLinkAppleCore
import Foundation
import Security
import XCTest
@testable import DeskLinkApp

final class HostAccessCredentialsTests: XCTestCase {
    func testPublicDeviceIDMatchesWindowsBigEndianFormulaAndGrouping() throws {
        let identity = HostIdentity(deviceID: Array(0..<16), secretKey: [UInt8](repeating: 7, count: 32))
        XCTAssertEqual(try HostPublicDeviceID.number(for: identity), 286_952_306_183)
        XCTAssertEqual(try HostPublicDeviceID.formatted(for: identity), "286 952 306 183")
        let changedSecretAndSuffix = HostIdentity(
            deviceID: Array(0..<8) + [UInt8](repeating: 255, count: 8),
            secretKey: [UInt8](repeating: 9, count: 32)
        )
        XCTAssertEqual(
            try HostPublicDeviceID.number(for: identity),
            try HostPublicDeviceID.number(for: changedSecretAndSuffix)
        )
    }

    func testPublicDeviceIDHandlesLargestPrefixAndRejectsTruncatedIdentity() throws {
        let largest = HostIdentity(deviceID: [UInt8](repeating: 255, count: 16), secretKey: [])
        XCTAssertEqual(try HostPublicDeviceID.number(for: largest), 373_709_551_615)
        let smallest = HostIdentity(deviceID: [UInt8](repeating: 0, count: 16), secretKey: [])
        XCTAssertEqual(try HostPublicDeviceID.formatted(for: smallest), "100 000 000 000")
        for length in [0, 7, 15, 17] {
            XCTAssertThrowsError(
                try HostPublicDeviceID.number(for: HostIdentity(deviceID: [UInt8](repeating: 7, count: length), secretKey: []))
            ) {
                XCTAssertEqual($0 as? HostAccessCredentialsError, .invalidDeviceID)
            }
        }
    }

    func testPasswordAcceptsAnyCharactersWithSufficientLength() throws {
        // Any characters are valid; only the byte length is bounded, and
        // surrounding whitespace is trimmed while inner bytes stay verbatim.
        XCTAssertEqual(try HostAccessPassword(" ABCDEFGH ").revealed, "ABCDEFGH")
        XCTAssertEqual(try HostAccessPassword("correct horse:电池 🐎!").revealed,
                       "correct horse:电池 🐎!")
        XCTAssertEqual(try HostAccessPassword("p@ssw0rd").bytes, Array("p@ssw0rd".utf8))
        for value in ["", "  ", "short", "1234567", String(repeating: "x", count: 65)] {
            XCTAssertThrowsError(try HostAccessPassword(value)) {
                XCTAssertEqual($0 as? HostAccessCredentialsError, .invalidPassword)
            }
        }
        XCTAssertThrowsError(try HostAccessPassword(bytes: Array("abcdefgh".utf8).dropLast()))
    }

    func testPasswordDescriptionsAndReflectionDoNotExposeValue() throws {
        let password = try HostAccessPassword("ABCDEFGH")
        XCTAssertFalse(String(describing: password).contains(password.revealed))
        XCTAssertFalse(String(reflecting: password).contains(password.revealed))
        var reflected = ""
        dump(password, to: &reflected)
        XCTAssertFalse(reflected.contains(password.revealed))
        XCTAssertTrue(reflected.contains("REDACTED"))
    }

    func testGeneratedPasswordsAreCanonical() throws {
        for _ in 0..<32 {
            let password = try HostAccessPassword.generate()
            XCTAssertEqual(password.bytes.count, 8)
            XCTAssertEqual(try HostAccessPassword(password.revealed), password)
        }
    }

    func testFixedPasswordRoundTripsReplacesAndClearsOnlyItsKeychainItem() throws {
        let keychain = InMemoryKeychainStore()
        let store = HostFixedAccessStore(keychain: keychain)
        try keychain.write(Data([99]), service: "unrelated", account: "other")
        XCTAssertNil(try store.load())
        let first = try HostAccessPassword("ABCDEFGH")
        try store.save(first)
        XCTAssertEqual(try store.load(), first)
        let second = try HostAccessPassword("correct horse:电池 staple!")
        try store.save(second)
        XCTAssertEqual(try store.load(), second)
        try store.clear()
        try store.clear()
        XCTAssertNil(try store.load())
        XCTAssertEqual(try keychain.read(service: "unrelated", account: "other"), Data([99]))
    }

    func testFixedRecordRejectsWrongVersionAndNoncanonicalPassword() throws {
        let valid = HostFixedAccessStore.encode(try HostAccessPassword("ABCDEFGH"))
        XCTAssertEqual(valid, Data([1]) + Data("ABCDEFGH".utf8))
        XCTAssertEqual(valid.first, HostFixedAccessStore.formatVersion)
        for invalid in [
            Data(), Data(valid.dropLast()),
            Data([2]) + Data(valid.dropFirst()),
            Data([1]),
            Data([1]) + Data("short".utf8),
            Data([1]) + Data(String(repeating: "x", count: 65).utf8),
        ] {
            XCTAssertThrowsError(try HostFixedAccessStore.decode(invalid)) {
                XCTAssertEqual($0 as? HostAccessCredentialsError, .malformedRecord)
            }
        }
    }

    func testCorruptStoredPasswordIsReportedWithoutReplacingIt() throws {
        let keychain = InMemoryKeychainStore()
        let corrupt = Data([2, 1, 2])
        try keychain.write(corrupt, service: HostFixedAccessStore.service, account: HostFixedAccessStore.account)
        XCTAssertThrowsError(try HostFixedAccessStore(keychain: keychain).load()) {
            XCTAssertEqual($0 as? HostAccessCredentialsError, .malformedRecord)
        }
        XCTAssertEqual(try keychain.read(service: HostFixedAccessStore.service, account: HostFixedAccessStore.account), corrupt)
    }

    func testFailedReplacementAndDeletionPreservePreviousPassword() throws {
        let keychain = FailingCredentialKeychain()
        let store = HostFixedAccessStore(keychain: keychain)
        let first = try HostAccessPassword("ABCDEFGH")
        try store.save(first)
        keychain.failure = .write
        XCTAssertThrowsError(try store.save(HostAccessPassword("23456789"))) {
            XCTAssertEqual($0 as? HostAccessCredentialsError, .keychain(errSecAuthFailed))
        }
        XCTAssertEqual(try store.load(), first)
        keychain.failure = .delete
        XCTAssertThrowsError(try store.clear()) {
            XCTAssertEqual($0 as? HostAccessCredentialsError, .keychain(errSecAuthFailed))
        }
        XCTAssertEqual(try store.load(), first)
        keychain.failure = .read
        XCTAssertThrowsError(try store.load()) {
            XCTAssertEqual($0 as? HostAccessCredentialsError, .keychain(errSecAuthFailed))
        }
    }
}

private final class FailingCredentialKeychain: KeychainStore, @unchecked Sendable {
    enum Failure { case read, write, delete }
    private let values = InMemoryKeychainStore()
    var failure: Failure?

    func read(service: String, account: String) throws -> Data? {
        if failure == .read { throw KeychainStoreError.status(errSecAuthFailed) }
        return try values.read(service: service, account: account)
    }

    func write(_ data: Data, service: String, account: String) throws {
        if failure == .write { throw KeychainStoreError.status(errSecAuthFailed) }
        try values.write(data, service: service, account: account)
    }

    func delete(service: String, account: String) throws {
        if failure == .delete { throw KeychainStoreError.status(errSecAuthFailed) }
        try values.delete(service: service, account: account)
    }
}
