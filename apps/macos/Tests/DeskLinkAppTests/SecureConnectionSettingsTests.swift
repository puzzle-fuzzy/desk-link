import XCTest
@testable import DeskLinkApp

final class SecureConnectionSettingsTests: XCTestCase {
    func testParsesExactDevelopmentConnectionCredentials() throws {
        let settings = try SecureConnectionSettings(environment: [
            "DESKLINK_RELAY_SERVER_NAME": "relay.example",
            "DESKLINK_SESSION_ID": String(repeating: "01", count: 16),
            "DESKLINK_AUTH_KEY": String(repeating: "a2", count: 32),
            "DESKLINK_HOST_VERIFY_KEY": String(repeating: "F3", count: 32),
        ])

        XCTAssertEqual(settings.serverName, "relay.example")
        XCTAssertEqual(settings.sessionID, [UInt8](repeating: 0x01, count: 16))
        XCTAssertEqual(settings.relayAuthentication, [UInt8](repeating: 0xa2, count: 32))
        XCTAssertEqual(settings.hostVerifyKey, [UInt8](repeating: 0xf3, count: 32))
    }

    func testRejectsMissingAndMalformedCredentials() {
        XCTAssertThrowsError(try SecureConnectionSettings(environment: [:])) {
            XCTAssertEqual($0 as? SecureConnectionSettingsError, .missing("DESKLINK_SESSION_ID"))
        }
        XCTAssertThrowsError(try SecureConnectionSettings(environment: [
            "DESKLINK_SESSION_ID": String(repeating: "00", count: 16),
            "DESKLINK_AUTH_KEY": "xyz",
            "DESKLINK_HOST_VERIFY_KEY": String(repeating: "00", count: 32),
        ])) {
            XCTAssertEqual(
                $0 as? SecureConnectionSettingsError,
                .invalidHex("DESKLINK_AUTH_KEY", expectedBytes: 32)
            )
        }
    }

    func testParsesSignedPairingInviteWithoutSeparateSessionSecrets() throws {
        let settings = try PairingInviteConnectionSettings(environment: [
            "DESKLINK_RELAY_SERVER_NAME": "relay.example",
            "DESKLINK_PAIRING_INVITE": String(repeating: "a5", count: 181),
        ])

        XCTAssertEqual(settings.serverName, "relay.example")
        XCTAssertEqual(settings.invite, [UInt8](repeating: 0xa5, count: 181))
    }

    func testRejectsMalformedSignedPairingInvite() {
        XCTAssertThrowsError(try PairingInviteConnectionSettings(environment: [
            "DESKLINK_PAIRING_INVITE": "00",
        ])) {
            XCTAssertEqual(
                $0 as? SecureConnectionSettingsError,
                .invalidHex("DESKLINK_PAIRING_INVITE", expectedBytes: 181)
            )
        }
    }
}
