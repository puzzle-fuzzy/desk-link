import XCTest
@testable import DeskLinkIOS

final class IOSBuildConfigurationTests: XCTestCase {
    func testProductionConfigurationIsExplicitlyControllerOnly() {
        XCTAssertEqual(IOSRuntimeConfiguration.production.platform, .ios)
        XCTAssertEqual(IOSRuntimeConfiguration.production.relayURL, "quic://p2p.yxswy.com:4433")
        XCTAssertEqual(IOSRuntimeConfiguration.production.relayServerName, "p2p.yxswy.com")
    }

    func testTestConfigurationUsesTheSharedAppleControllerCore() {
        let configuration = IOSRuntimeConfiguration.test
        XCTAssertEqual(configuration.platform, .ios)
        XCTAssertEqual(configuration.relayServerName, "localhost")
    }
}
