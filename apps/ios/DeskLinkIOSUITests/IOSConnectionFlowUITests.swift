import XCTest

final class IOSConnectionFlowUITests: XCTestCase {
    func testLoginCanBeSkippedBeforeOpeningTheRemoteWorkspace() {
        let app = XCUIApplication()
        app.launch()

        let skipButton = app.buttons["跳过登录，直接使用"]
        XCTAssertTrue(skipButton.waitForExistence(timeout: 5))
        skipButton.tap()
        XCTAssertTrue(app.tabBars.buttons["连接设备"].waitForExistence(timeout: 5))
    }

    func testScanButtonOpensScannerInsteadOfPasting() {
        let app = XCUIApplication()
        app.launch()

        let skipButton = app.buttons["跳过登录，直接使用"]
        if skipButton.waitForExistence(timeout: 5) {
            skipButton.tap()
            XCTAssertTrue(app.tabBars.buttons["连接设备"].waitForExistence(timeout: 5))
        }

        let scanButton = app.buttons["scan-invite"]
        XCTAssertTrue(scanButton.waitForExistence(timeout: 5))
        scanButton.tap()

        XCTAssertTrue(app.navigationBars["扫描二维码"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["取消"].exists)
        XCTAssertFalse(app.staticTexts["剪贴板中没有连接码。"].exists)
    }
}
