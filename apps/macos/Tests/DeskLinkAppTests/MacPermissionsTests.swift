import XCTest
@testable import DeskLinkApp

final class MacPermissionsTests: XCTestCase {
    func testSnapshotMapsInjectableProviderAndIncludesActionableSettingsURLs() {
        let permissions = MacPermissions(provider: StaticMacPermissionProvider(
            screenRecordingGranted: false,
            accessibilityGranted: true
        ))

        let snapshot = permissions.snapshot()

        XCTAssertEqual(snapshot.screenRecording, .denied)
        XCTAssertEqual(snapshot.accessibility, .granted)
        XCTAssertEqual(snapshot.screenRecordingSettingsURL.scheme, "x-apple.systempreferences")
        XCTAssertEqual(snapshot.accessibilitySettingsURL.scheme, "x-apple.systempreferences")
        XCTAssertFalse(snapshot.canCaptureAndControl)
    }
}
