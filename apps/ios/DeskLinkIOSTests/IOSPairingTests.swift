import XCTest
@testable import DeskLinkIOS

final class IOSPairingTests: XCTestCase {
    func testPasteAndQRCodeUseTheSameInviteDecoder() {
        let encoded = Data(repeating: 7, count: 181).base64EncodedString()

        XCTAssertEqual(IOSPairingInput.decodeInvite("  \(encoded)\n"), Data(repeating: 7, count: 181))
        XCTAssertNil(IOSPairingInput.decodeInvite("not-a-desklink-invite"))
    }

    func testIOSNavigationDoesNotExposeHostMode() {
        XCTAssertFalse(IOSRootDestination.allCases.map(\.rawValue).contains("共享此设备"))
        XCTAssertEqual(IOSRootDestination.allCases, [.connect, .savedHosts, .more])
    }
}
