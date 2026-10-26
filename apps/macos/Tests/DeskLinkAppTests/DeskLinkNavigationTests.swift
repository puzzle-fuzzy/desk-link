import Foundation
import XCTest
import DeskLinkAppleCore
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
        XCTAssertNotEqual(DeskLinkSection.connect.rawValue, "本机状态")
    }

    func testQRCodePayloadRemainsTheInviteBase64ExpectedByIOS() {
        let invite = Data([0x00, 0x01, 0x7f, 0xff])

        let payload = DeskLinkQRCode.payload(for: invite)

        XCTAssertEqual(payload, invite.base64EncodedString())
        XCTAssertEqual(Data(base64Encoded: payload), invite)
    }

    func testMacOSDefaultsUseTheManagedRelayOutsideLoopbackTests() {
        guard ProcessInfo.processInfo.environment["DESKLINK_RELAY_URL"] == nil,
              ProcessInfo.processInfo.environment["DESKLINK_RELAY_SERVER_NAME"] == nil
        else { return }
        XCTAssertEqual(DeskLinkRuntimeConfiguration.macOSDefaults.relayURL, "quic://turn.p2p.yxswy.com:4433")
        XCTAssertEqual(DeskLinkRuntimeConfiguration.macOSDefaults.relayServerName, "turn.p2p.yxswy.com")
    }

    func testRelayFailureDoesNotLookLikeASecurityValidationFailure() {
        XCTAssertEqual(
            deskLinkChineseError("The relay could not start the host invitation. (code 4)."),
            "中继服务暂时不可用，请检查网络或中继配置后重试。"
        )
    }

    func testPrimaryNavigationUsesRemoteTasks() {
        XCTAssertEqual(
            DeskLinkSection.allCases.map(\.rawValue),
            ["连接设备", "共享此设备", "设置与诊断"]
        )
        XCTAssertEqual(DeskLinkSection.connect.rawValue, "连接设备")
    }

    func testHostStatusSummaryKeepsRuntimeDetailsOutOfTheTopLevelCopy() {
        let summary = deskLinkHostStatus(for: .idle, lastError: nil)

        XCTAssertEqual(summary.title, "未开启共享")
        XCTAssertEqual(summary.detail, "需要共享这台 Mac 时开启共享")
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

    func testEveryWaitingPhaseOffersCancellationAndExplainsTheNextStep() {
        let waitingStates: [ConnectionState] = [.connecting, .pairing, .reconnecting, .recovering]
        for state in waitingStates {
            XCTAssertTrue(deskLinkConnectionIsWaiting(state))
            XCTAssertTrue(deskLinkConnectionWaitingDetail(state)?.contains("取消") == true)
        }

        let otherStates: [ConnectionState] = [.idle, .closed, .failed("offline"), .connected(streamID: 7), .frozen]
        for state in otherStates {
            XCTAssertFalse(deskLinkConnectionIsWaiting(state))
            XCTAssertNil(deskLinkConnectionWaitingDetail(state))
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


extension DeskLinkNavigationTests {
    func testConnectionProgressUsesActualRuntimeStagesAndProvidesDelayedGuidance() {
        let stages: [(ControllerConnectionStage, Int?)] = [
            (.idle, nil), (.creatingSession, 0), (.connectingRelay, 0),
            (.secureHandshake, 1), (.waitingApproval, 2),
            (.negotiating, 3), (.startingVideo, 3), (.connected, 3),
            (.degraded, 1), (.recoveringVideo, 1), (.reconnecting, 1),
            (.disconnecting, nil), (.closed, nil),
        ]
        for (stage, step) in stages {
            XCTAssertEqual(deskLinkConnectionProgress(for: stage, elapsed: 0).activeStep, step)
        }
        XCTAssertTrue(deskLinkConnectionProgress(for: .waitingApproval, elapsed: 16).guidance.contains("取消后重试"))
        XCTAssertFalse(deskLinkConnectionProgress(for: .waitingApproval, elapsed: 1).guidance.contains("取消后重试"))
    }

    func testConnectionElapsedBoundsNegativeTimeAndFormatsLongWaits() {
        XCTAssertEqual(deskLinkConnectionElapsed(-1), "刚刚开始")
        XCTAssertEqual(deskLinkConnectionElapsed(3.8), "已等待 3 秒")
        XCTAssertEqual(deskLinkConnectionElapsed(60), "已等待 1 分钟")
        XCTAssertEqual(deskLinkConnectionElapsed(125), "已等待 2 分 5 秒")
    }

    func testRecentDeviceTitlePrefersAliasThenDeviceID() {
        let aliasHost = recentDevice(deviceID: "123456789012", displayName: "办公室电脑")
        XCTAssertEqual(deskLinkSavedHostTitle(aliasHost), "办公室电脑")
        XCTAssertEqual(deskLinkSavedHostDeviceID(aliasHost), "123 456 789 012")
        XCTAssertEqual(deskLinkSavedHostTitle(recentDevice(deviceID: "123456789012")), "123 456 789 012")
    }

    func testLegacyDevicesOnSameRelayHaveDistinctNonSecretLabels() {
        let first = recentDevice(key: 1)
        let second = recentDevice(key: 2)
        XCTAssertNotEqual(deskLinkSavedHostTitle(first), deskLinkSavedHostTitle(second))
        XCTAssertFalse(deskLinkSavedHostTitle(first).contains(first.serverName))
        XCTAssertEqual(deskLinkSavedHostIdentity(first).count, 12)
        let rotatedCredentials = SavedHost(
            id: UUID(), serverName: first.serverName,
            sessionID: [UInt8](repeating: 9, count: 16),
            relayAuthentication: [UInt8](repeating: 8, count: 32),
            hostVerifyKey: first.hostVerifyKey
        )
        XCTAssertEqual(deskLinkSavedHostTitle(first), deskLinkSavedHostTitle(rotatedCredentials))
    }

    private func recentDevice(key: UInt8 = 3, deviceID: String? = nil, displayName: String? = nil) -> SavedHost {
        SavedHost(
            id: UUID(), serverName: "relay.example.com",
            sessionID: [UInt8](repeating: 1, count: 16),
            relayAuthentication: [UInt8](repeating: 2, count: 32),
            hostVerifyKey: [UInt8](repeating: key, count: 32),
            deviceID: deviceID, displayName: displayName
        )
    }
}
