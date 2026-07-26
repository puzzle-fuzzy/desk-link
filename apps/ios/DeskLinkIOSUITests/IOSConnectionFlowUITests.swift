import XCTest

final class IOSConnectionFlowUITests: XCTestCase {
    func testConnectionFlowDoesNotExposeHostMode() {
        let app = XCUIApplication()
        app.launch()

        XCTAssertTrue(app.tabBars.buttons["连接设备"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.tabBars.buttons["已保存设备"].exists)
        XCTAssertTrue(app.tabBars.buttons["更多"].exists)
        XCTAssertFalse(app.staticTexts["共享此设备"].exists)
    }

    func testScanButtonOpensScannerInsteadOfPasting() {
        let app = XCUIApplication()
        app.launch()

        let scanButton = app.buttons["scan-invite"]
        XCTAssertTrue(scanButton.waitForExistence(timeout: 5))
        scanButton.tap()

        XCTAssertTrue(app.navigationBars["扫描二维码"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["取消"].exists)
        XCTAssertFalse(app.staticTexts["剪贴板中没有连接码。"].exists)
    }
}
