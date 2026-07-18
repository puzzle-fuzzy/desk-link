import XCTest
@testable import DeskLinkApp

final class DeskLinkNavigationTests: XCTestCase {
    private let grantedPermissions = MacPermissionSnapshot(
        screenRecording: .granted,
        accessibility: .granted,
        screenRecordingSettingsURL: MacPermissions.screenRecordingSettingsURL,
        accessibilitySettingsURL: MacPermissions.accessibilitySettingsURL
    )

    func testConnectionWorkspaceCopyNamesTheRemoteTask() {
        XCTAssertEqual("连接设备", DeskLinkSection.connect.rawValue)
        XCTAssertEqual("共享此设备", DeskLinkSection.share.rawValue)
        XCTAssertNotEqual(DeskLinkSection.connect.rawValue, "本机状态")
    }

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

    func testHostStatusExplainsEachMissingPermission() {
        let cases: [(MacPermissionStatus, MacPermissionStatus, String)] = [
            (.denied, .granted, "在系统设置中允许屏幕录制"),
            (.granted, .denied, "在系统设置中允许辅助功能"),
            (.denied, .denied, "在系统设置中允许屏幕录制与辅助功能"),
        ]

        for (screenRecording, accessibility, expectedDetail) in cases {
            let permissions = MacPermissionSnapshot(
                screenRecording: screenRecording,
                accessibility: accessibility,
                screenRecordingSettingsURL: MacPermissions.screenRecordingSettingsURL,
                accessibilitySettingsURL: MacPermissions.accessibilitySettingsURL
            )
            let summary = deskLinkHostStatus(
                for: .idle,
                permissions: permissions,
                hasPendingApproval: false,
                lastError: nil
            )

            XCTAssertEqual(summary.title, "需要处理")
            XCTAssertEqual(summary.detail, expectedDetail)
        }
    }

    func testHostStatusSurfacesPendingApproval() {
        let summary = deskLinkHostStatus(
            for: .waitingForApproval,
            permissions: grantedPermissions,
            hasPendingApproval: true,
            lastError: nil
        )

        XCTAssertEqual(summary.title, "等待确认")
        XCTAssertEqual(summary.detail, "有设备请求控制这台 Mac，请允许或拒绝")
    }

    func testConnectedHostStatusDescribesTheActiveSession() {
        let summary = deskLinkHostStatus(
            for: .connected,
            permissions: grantedPermissions,
            hasPendingApproval: false,
            lastError: nil
        )

        XCTAssertEqual(summary.title, "正在共享本机")
        XCTAssertEqual(summary.detail, "远程设备正在查看并控制这台 Mac")
        XCTAssertFalse(summary.detail.contains("视频"))
        XCTAssertFalse(summary.detail.contains("密钥"))
    }

    func testConnectionStatusMapsEveryRuntimeState() {
        let cases: [(ConnectionState, String)] = [
            (.idle, "准备连接"),
            (.closed, "准备连接"),
            (.pairing, "等待确认"),
            (.connecting, "连接中"),
            (.connected(streamID: 7), "已连接"),
            (.reconnecting, "正在恢复连接"),
            (.recovering, "正在恢复连接"),
            (.frozen, "画面暂时冻结"),
            (.failed("relay unavailable"), "连接失败"),
        ]

        for (state, expectedTitle) in cases {
            XCTAssertEqual(deskLinkConnectionStatus(for: state).title, expectedTitle)
        }
    }

    func testActiveSessionUsesConnectionStatusTitlesWithoutStreamDetails() {
        let cases: [(ConnectionState, String)] = [
            (.connected(streamID: 7), "已连接"),
            (.reconnecting, "正在恢复连接"),
            (.recovering, "正在恢复连接"),
            (.frozen, "画面暂时冻结"),
            (.failed("relay unavailable"), "连接失败"),
        ]

        for (state, expectedTitle) in cases {
            XCTAssertEqual(deskLinkSessionStatusText(for: state), expectedTitle)
        }
    }

    func testActiveSessionKeepsOnlyReleaseSafetyCopyOutsideDiagnostics() {
        XCTAssertEqual(deskLinkSessionSafetyCopy, "退出窗口前，DeskLink 会释放所有按键与鼠标状态。")
        XCTAssertFalse(deskLinkSessionSafetyCopy.contains("帧"))
    }

    func testWindowApprovalPresenterRemainsAvailableDuringControllerSessions() {
        let approval = HostApproval(
            id: UUID(),
            fingerprint: "controller-fingerprint",
            controllerDeviceID: [0x01],
            controllerVerifyKey: [0x02]
        )

        let sessionStates: [ConnectionState] = [
            .connected(streamID: 7),
            .reconnecting,
            .recovering,
            .frozen,
        ]

        for state in sessionStates {
            XCTAssertEqual(
                deskLinkApprovalForWindowPresentation(approval, controllerState: state),
                approval,
                "A host approval must remain presentable while \(state) replaces the shell with SessionView."
            )
        }
    }
}
