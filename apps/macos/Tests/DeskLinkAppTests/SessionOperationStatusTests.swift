import Foundation
import DeskLinkAppleCore
import XCTest
@testable import DeskLinkApp

final class SessionOperationStatusTests: XCTestCase {
    func testQueueAdmissionRemainsPendingUntilMatchingRemoteConfirmation() throws {
        var status = SessionOperationStatus()
        status.qualityQueued(.sharp)
        XCTAssertEqual(status.pendingQuality, .sharp)
        XCTAssertTrue(status.hasPending)
        XCTAssertTrue(status.message?.contains("等待远端确认") == true)
        XCTAssertFalse(status.message?.contains("已确认") == true)
        status.qualityConfirmed(try quality(preference: 3, preset: 2))
        XCTAssertFalse(status.hasPending)
        XCTAssertTrue(status.message?.contains("远端已确认") == true)
    }

    func testDifferentRemoteQualityReportsActualStateAndAllowsRetry() throws {
        var status = SessionOperationStatus()
        status.qualityQueued(.sharp)
        status.qualityConfirmed(try quality(preference: 0, preset: 0))
        XCTAssertFalse(status.hasPending)
        XCTAssertTrue(status.message?.contains("远端仍使用自动") == true)
        XCTAssertTrue(status.message?.contains("重试") == true)
    }

    func testFailedAdmissionDoesNotClaimThatHostAppliedChange() {
        var status = SessionOperationStatus()
        status.couldNotSend("操作暂未发送，请稍后重试。")
        XCTAssertFalse(status.hasPending)
        XCTAssertEqual(status.message, "操作暂未发送，请稍后重试。")
    }

    func testConfirmationTimeoutRemainsRetryableAndDoesNotReportSuccess() {
        var status = SessionOperationStatus()
        status.qualityQueued(.balanced)
        status.qualityTimedOut()
        XCTAssertFalse(status.hasPending)
        XCTAssertTrue(status.message?.contains("尚未确认") == true)
        status.displayQueued(300)
        status.displayTimedOut()
        XCTAssertFalse(status.hasPending)
        XCTAssertTrue(status.message?.contains("尚未确认") == true)
    }

    func testDisplaySelectionWaitsForActualActiveDisplayAndHandlesRefusal() {
        var status = SessionOperationStatus()
        status.displayQueued(300)
        status.displayConfirmed(nil)
        XCTAssertTrue(status.hasPending)
        status.displayConfirmed(7)
        XCTAssertFalse(status.hasPending)
        XCTAssertTrue(status.message?.contains("仍使用原显示器") == true)
        status.displayQueued(300)
        status.displayConfirmed(300)
        XCTAssertFalse(status.hasPending)
        XCTAssertEqual(status.message, "远端已确认切换显示器。")
    }

    func testResetDiscardsPendingOperationFromPreviousConnection() {
        var status = SessionOperationStatus()
        status.qualityQueued(.smooth)
        status.reset()
        XCTAssertFalse(status.hasPending)
        XCTAssertNil(status.message)
        status.qualityTimedOut()
        XCTAssertNil(status.message)
    }

    private func quality(preference: UInt8, preset: UInt8) throws -> SessionVideoQuality {
        let state = try XCTUnwrap(SessionControlState(encoded: Data([10, preference, preset])))
        guard case let .videoQuality(value) = state else { throw NSError(domain: "test", code: 1) }
        return value
    }
}
