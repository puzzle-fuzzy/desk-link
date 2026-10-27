import XCTest
@testable import DeskLinkApp

final class HostAccessPresentationTests: XCTestCase {
    private let granted = MacPermissionSnapshot(
        screenRecording: .granted, accessibility: .granted,
        screenRecordingSettingsURL: MacPermissions.screenRecordingSettingsURL,
        accessibilitySettingsURL: MacPermissions.accessibilitySettingsURL)

    func testPasswordConfirmationRequiresExactMatchAndSufficientLength() {
        // Passwords are verbatim: case and separators matter on both peers.
        XCTAssertNil(HostFixedPasswordValidation.message(password: "abcd-2345", confirmation: "abcd-2345"))
        XCTAssertNil(HostFixedPasswordValidation.message(password: "correct horse:电池 🐎!", confirmation: "correct horse:电池 🐎!"))
        XCTAssertNotNil(HostFixedPasswordValidation.message(password: "abcd-2345", confirmation: "ABCD 2345"))
        XCTAssertNotNil(HostFixedPasswordValidation.message(password: "ABCD2345", confirmation: "ABCD2346"))
        XCTAssertNotNil(HostFixedPasswordValidation.message(password: "ABCD2340", confirmation: "short"))
    }

    func testRelayReadyIsNotDescribedAsConnecting() {
        let status = deskLinkHostStatus(for: .connecting, permissions: granted,
            hasPendingApproval: false, lastError: nil, sharingStatus: .ready)
        XCTAssertEqual(status.title, "共享已开启")
        XCTAssertEqual(status.tone, .ready)
    }

    func testPendingApprovalAndPermissionsTakePriorityOverDirectoryReady() {
        let pending = deskLinkHostStatus(for: .waitingForApproval, permissions: granted,
            hasPendingApproval: true, lastError: nil, sharingStatus: .ready)
        XCTAssertEqual(pending.title, "等待确认")
        let denied = deskLinkHostStatus(for: .connecting, permissions: .denied,
            hasPendingApproval: false, lastError: nil, sharingStatus: .ready)
        XCTAssertEqual(denied.tone, .attention)
    }

    func testDirectoryStoppingAndFailureRemainActionable() {
        let stopping = deskLinkHostStatus(for: .stopping, permissions: granted,
            hasPendingApproval: false, lastError: nil, sharingStatus: .stopping)
        XCTAssertEqual(stopping.title, "正在停止共享")
        let failed = deskLinkHostStatus(for: .closed, permissions: granted,
            hasPendingApproval: false, lastError: nil, sharingStatus: .failed("registration failed"))
        XCTAssertEqual(failed.tone, .attention)
    }
}
