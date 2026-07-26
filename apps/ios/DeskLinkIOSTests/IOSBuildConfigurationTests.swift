import XCTest
@testable import DeskLinkIOS

final class IOSBuildConfigurationTests: XCTestCase {
    func testProductionConfigurationIsExplicitlyControllerOnly() {
        XCTAssertEqual(IOSRuntimeConfiguration.production.platform, .ios)
    }

    func testTestConfigurationUsesTheSharedAppleControllerCore() {
        let configuration = IOSRuntimeConfiguration.test
        XCTAssertEqual(configuration.platform, .ios)
        XCTAssertEqual(configuration.relayServerName, "localhost")
    }
}
