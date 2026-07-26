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
}
