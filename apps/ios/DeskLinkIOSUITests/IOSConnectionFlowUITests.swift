import XCTest

final class IOSConnectionFlowUITests: XCTestCase {
    func testLoginIsRequiredBeforeOpeningTheRemoteWorkspace() {
        let app = XCUIApplication()
        app.launch()

        XCTAssertTrue(app.staticTexts["登录 DeskLink"].waitForExistence(timeout: 5))
        XCTAssertFalse(app.tabBars.buttons["连接设备"].exists)
    }

    func testScanButtonOpensScannerInsteadOfPasting() {
        let app = XCUIApplication()
        app.launchArguments += ["-DeskLinkUITestSignedIn"]
        app.launch()

        let scanButton = app.buttons["scan-invite"]
        XCTAssertTrue(scanButton.waitForExistence(timeout: 5))
        scanButton.tap()

        XCTAssertTrue(app.navigationBars["扫描二维码"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["取消"].exists)
        XCTAssertFalse(app.staticTexts["剪贴板中没有连接码。"].exists)
    }
}
