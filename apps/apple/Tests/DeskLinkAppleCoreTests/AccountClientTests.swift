import Foundation
import XCTest
@testable import DeskLinkAppleCore

@MainActor
final class AccountClientTests: XCTestCase {
    func testSkipLoginEntersLocalOnlyStateWithoutPersistingSession() {
        let keychain = InMemoryKeychainStore()
        let client = AccountClient(
            baseURL: URL(string: "https://account.example.com")!,
            platform: .macos,
            deviceName: "Test Mac",
            keychain: keychain
        )

        client.skipLogin()

        XCTAssertEqual(client.state, .skipped)
        XCTAssertNil(client.lastError)
        XCTAssertNil(try? keychain.read(service: "com.desklink.account-session", account: "current"))
    }
}
