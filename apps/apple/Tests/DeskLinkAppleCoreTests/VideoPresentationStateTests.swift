import Foundation
import XCTest
@testable import DeskLinkAppleCore

final class VideoPresentationStateTests: XCTestCase {
    private func context(stream: UInt64 = 4, epoch: UInt64 = 2, version: UInt32 = 3, generation: UInt64 = 5) -> CapturedInputContext {
        CapturedInputContext(streamID: stream, inputEpoch: epoch, configVersion: version, runtimeGeneration: generation)
    }

    private func token(_ context: CapturedInputContext, frame: UInt64 = 7) -> VideoFramePresentationToken {
        VideoFramePresentationToken(context: context, frameID: frame, width: 1920, height: 1080)
    }

    func testActualReceiptMayAcknowledgeAnEarlierIssuedFrameWithinCurrentConfiguration() {
        var state = VideoPresentationState()
        let current = context()
        XCTAssertNil(state.context)
        XCTAssertTrue(state.confirm(token(current, frame: 7), current: current, newestIssuedFrameID: 10))
        XCTAssertEqual(state.context, current)
        XCTAssertEqual(state.frameID, 7)
        // Re-presenting the same pixels after zoom/layout may confirm new rects.
        XCTAssertTrue(state.confirm(token(current, frame: 7), current: current, newestIssuedFrameID: 11))
        XCTAssertFalse(state.confirm(token(current, frame: 6), current: current, newestIssuedFrameID: 11))
    }

    func testOldStreamEpochVersionOrRuntimeReceiptCannotActivateSuccessor() {
        let current = context()
        for stale in [context(stream: 3), context(epoch: 1), context(version: 2), context(generation: 4)] {
            var state = VideoPresentationState()
            XCTAssertFalse(state.confirm(token(stale), current: current, newestIssuedFrameID: 10))
            XCTAssertNil(state.context)
        }
    }

    func testResetAndNewTopologyDoNotResurrectOldContext() {
        var state = VideoPresentationState()
        let old = context()
        XCTAssertTrue(state.confirm(token(old), current: old, newestIssuedFrameID: 10))
        state.reset()
        XCTAssertNil(state.context)
        XCTAssertEqual(state.frameID, 0)
        let successor = context(epoch: 3, version: 4)
        XCTAssertFalse(state.confirm(token(old), current: successor, newestIssuedFrameID: 12))
        XCTAssertTrue(state.confirm(token(successor, frame: 11), current: successor, newestIssuedFrameID: 12))
    }

    func testUnissuedFramesAndZeroEpochNeverActivateInput() {
        var state = VideoPresentationState()
        let current = context()
        XCTAssertFalse(state.confirm(token(current, frame: 11), current: current, newestIssuedFrameID: 10))
        XCTAssertFalse(state.confirm(token(current, frame: 0), current: current, newestIssuedFrameID: 10))
        let zero = context(epoch: 0)
        XCTAssertFalse(state.confirm(token(zero), current: zero, newestIssuedFrameID: 10))
        XCTAssertNil(state.context)
    }
}
