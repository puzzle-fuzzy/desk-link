import XCTest

final class IOSConnectionFlowUITests: XCTestCase {
    func testWorkspaceOpensDirectlyWithConnectionTab() {
        let app = XCUIApplication()
        app.launch()

        // Phase 1-2 ships a local-mode workspace without an account gate.
        XCTAssertTrue(app.tabBars.buttons["连接设备"].waitForExistence(timeout: 5))
    }

    func testScanButtonOpensScannerInsteadOfPasting() {
        let app = XCUIApplication()
        app.launch()

        XCTAssertTrue(app.tabBars.buttons["连接设备"].waitForExistence(timeout: 5))

        let scanButton = app.buttons["scan-invite"]
        XCTAssertTrue(scanButton.waitForExistence(timeout: 5))
        scanButton.tap()

        XCTAssertTrue(app.navigationBars["扫描二维码"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["取消"].exists)
        XCTAssertFalse(app.staticTexts["剪贴板中没有连接码。"].exists)
    }
}
