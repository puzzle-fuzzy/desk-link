import XCTest
@testable import DeskLinkAppleCore

final class CapturedInputContextTests: XCTestCase {
    private let original = CapturedInputContext(streamID: 7, inputEpoch: 11, configVersion: 2, runtimeGeneration: 3)

    func testQualityReconfigurationKeepsTargetButRetainsDifferentFullContext() {
        let quality = CapturedInputContext(streamID: 7, inputEpoch: 11, configVersion: 3, runtimeGeneration: 3)
        XCTAssertTrue(original.hasSameInputTarget(as: quality))
        XCTAssertEqual(original.targetIdentity, quality.targetIdentity)
        XCTAssertNotEqual(original, quality)
        XCTAssertEqual(Set([original, quality]).count, 2)
        XCTAssertEqual(Set([original.targetIdentity, quality.targetIdentity]).count, 1)
    }

    func testStreamEpochOrRuntimeChangeIsANewTarget() {
        for changed in [
            CapturedInputContext(streamID: 8, inputEpoch: 11, configVersion: 2, runtimeGeneration: 3),
            CapturedInputContext(streamID: 7, inputEpoch: 12, configVersion: 2, runtimeGeneration: 3),
            CapturedInputContext(streamID: 7, inputEpoch: 11, configVersion: 2, runtimeGeneration: 4),
        ] {
            XCTAssertFalse(original.hasSameInputTarget(as: changed))
        }
    }
}
