import XCTest
@testable import DeskLinkApp

final class DeskLinkNavigationTests: XCTestCase {
    func testPrimaryNavigationUsesRemoteTasks() {
        XCTAssertEqual(
            DeskLinkSection.allCases.map(\.rawValue),
            ["连接设备", "共享此设备", "已批准设备", "设置 / 诊断"]
        )
        XCTAssertEqual(DeskLinkSection.connect.rawValue, "连接设备")
    }

    func testHostStatusSummaryKeepsRuntimeDetailsOutOfTheTopLevelCopy() {
        let summary = deskLinkHostStatus(for: .idle, lastError: nil)

        XCTAssertEqual(summary.title, "未开启共享")
        XCTAssertEqual(summary.detail, "需要共享这台 Mac 时生成连接码")
        XCTAssertFalse(summary.detail.contains("视频"))
        XCTAssertFalse(summary.detail.contains("关键帧"))
    }

    func testHostErrorTakesPriorityOverIdleState() {
        let summary = deskLinkHostStatus(for: .idle, lastError: "权限检查失败")

        XCTAssertEqual(summary.title, "需要处理")
        XCTAssertEqual(summary.detail, "打开设置检查本机共享权限")
    }
}
