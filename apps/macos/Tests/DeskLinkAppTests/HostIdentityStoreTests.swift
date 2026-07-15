import Foundation
import XCTest
@testable import DeskLinkApp

final class HostIdentityStoreTests: XCTestCase {
    func testHostIdentityRecordHasVersionedFixedLayout() throws {
        let identity = HostIdentity(
            deviceID: [UInt8](repeating: 0x11, count: HostIdentity.deviceIDLength),
            secretKey: [UInt8](repeating: 0x22, count: HostIdentity.secretKeyLength)
        )

        let encoded = try HostIdentityStore.encode(identity)

        XCTAssertEqual(encoded.count, HostIdentityStore.recordLength)
        XCTAssertEqual(encoded.first, HostIdentityStore.formatVersion)
        XCTAssertEqual(try HostIdentityStore.decode(encoded), identity)
    }

    func testTrustedControllerRecordsRejectDuplicateDeviceIDsAndMalformedData() throws {
        let first = TrustedController(
            deviceID: [UInt8](repeating: 0x01, count: 16),
            verifyKey: [UInt8](repeating: 0x02, count: 32),
            approvedAtUnixSeconds: 123,
            displayName: "Primary Mac"
        )
        let duplicateID = TrustedController(
            deviceID: [UInt8](repeating: 0x01, count: 16),
            verifyKey: [UInt8](repeating: 0x03, count: 32),
            approvedAtUnixSeconds: 456,
            displayName: "Other Mac"
        )

        XCTAssertThrowsError(try TrustedControllerStore.encode([first, duplicateID])) {
            XCTAssertEqual($0 as? TrustedControllerStoreError, .duplicateDeviceID)
        }
        XCTAssertThrowsError(try TrustedControllerStore.decode(Data([TrustedControllerStore.formatVersion]))) {
            XCTAssertEqual($0 as? TrustedControllerStoreError, .malformedRecord)
        }
    }

    func testTrustedControllerRecordsRejectExactDuplicatesAndNonzeroPadding() throws {
        let controller = TrustedController(
            deviceID: [UInt8](repeating: 0x11, count: 16),
            verifyKey: [UInt8](repeating: 0x22, count: 32),
            approvedAtUnixSeconds: 123,
            displayName: "Desk"
        )

        XCTAssertThrowsError(try TrustedControllerStore.encode([controller, controller])) {
            XCTAssertEqual($0 as? TrustedControllerStoreError, .duplicateRecord)
        }
        var encoded = try TrustedControllerStore.encode([controller])
        encoded[encoded.index(before: encoded.endIndex)] = 0xff
        XCTAssertThrowsError(try TrustedControllerStore.decode(encoded)) {
            XCTAssertEqual($0 as? TrustedControllerStoreError, .malformedRecord)
        }
    }
}
