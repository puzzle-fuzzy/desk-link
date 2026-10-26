import Foundation
import XCTest
@testable import DeskLinkAppleCore

final class H264DecodeSchedulerTests: XCTestCase {
    private func makeScheduler(queue: DispatchQueue, limits: H264DecodeScheduler.Limits) -> H264DecodeScheduler {
        H264DecodeScheduler(limits: limits, decodeQueue: queue, onFrame: { _ in },
                            onConfiguration: { _, _, _, _, _ in }, onKeyframeRequest: { _, _ in })
    }

    func testCongestionDropsWholeDependentChainAndWaitsForIDR() {
        let queue = DispatchQueue(label: "test.decoder.paused")
        queue.suspend()
        defer { queue.resume() }
        let scheduler = makeScheduler(queue: queue, limits: .init(frames: 2, bytes: 64))
        scheduler.reset(generation: 7)
        scheduler.configure(sequenceHeader: Data([1]), streamID: 2, width: 10, height: 10, version: 1, generation: 7)
        let idr = Data([0, 0, 0, 1, 0x65, 1])
        let delta = Data([0, 0, 0, 1, 0x41, 1])
        scheduler.receive(accessUnit: idr, streamID: 2, frameID: UInt64.max, version: 1, generation: 7)
        XCTAssertEqual(scheduler.snapshot().queuedFrames, 0)
        scheduler.receive(accessUnit: idr, streamID: 2, frameID: 1, version: 1, generation: 7)
        scheduler.receive(accessUnit: delta, streamID: 2, frameID: 2, version: 1, generation: 7)
        XCTAssertEqual(scheduler.snapshot().queuedFrames, 2)
        scheduler.receive(accessUnit: delta, streamID: 2, frameID: 3, version: 1, generation: 7)
        XCTAssertEqual(scheduler.snapshot().queuedFrames, 0)
        XCTAssertEqual(scheduler.snapshot().queuedBytes, 0)
        XCTAssertEqual(scheduler.snapshot().droppedFrames, 3)
        XCTAssertTrue(scheduler.snapshot().awaitingKeyframe)
        scheduler.receive(accessUnit: idr, streamID: 2, frameID: 4, version: 1, generation: 7)
        XCTAssertEqual(scheduler.snapshot().queuedFrames, 1)
        XCTAssertFalse(scheduler.snapshot().awaitingKeyframe)
    }

    func testResetAndReconfigurationRejectOldGenerationAndVersion() {
        let queue = DispatchQueue(label: "test.decoder.identity")
        queue.suspend()
        defer { queue.resume() }
        let scheduler = makeScheduler(queue: queue, limits: .init())
        scheduler.reset(generation: 8)
        scheduler.configure(sequenceHeader: Data([1]), streamID: 1, width: 10, height: 10, version: 1, generation: 7)
        XCTAssertFalse(scheduler.accepts(generation: 7, streamID: 1, version: 1))
        scheduler.configure(sequenceHeader: Data([1]), streamID: 2, width: 10, height: 10, version: 2, generation: 8)
        scheduler.receive(accessUnit: Data([0, 0, 1, 0x65, 1]), streamID: 2, frameID: 1, version: 1, generation: 8)
        XCTAssertEqual(scheduler.snapshot().queuedFrames, 0)
        XCTAssertTrue(scheduler.accepts(generation: 8, streamID: 2, version: 2))
        scheduler.configure(sequenceHeader: Data([1]), streamID: 3, width: 10, height: 10, version: 1, generation: 8)
        XCTAssertFalse(scheduler.accepts(generation: 8, streamID: 2, version: 2))
        XCTAssertTrue(scheduler.accepts(generation: 8, streamID: 3, version: 1))
    }

    func testByteBudgetAndAgeInvalidateReferenceChain() {
        let queue = DispatchQueue(label: "test.decoder.age")
        queue.suspend()
        defer { queue.resume() }
        let scheduler = makeScheduler(queue: queue, limits: .init(frames: 6, bytes: 8, ageNanoseconds: 1))
        scheduler.reset(generation: 1)
        scheduler.configure(sequenceHeader: Data([1]), streamID: 1, width: 10, height: 10, version: 1, generation: 1)
        scheduler.receive(accessUnit: Data([0, 0, 1, 0x65, 1]), streamID: 1, frameID: 1, version: 1, generation: 1)
        scheduler.receive(accessUnit: Data([0, 0, 1, 0x41, 1]), streamID: 1, frameID: 2, version: 1, generation: 1)
        XCTAssertEqual(scheduler.snapshot().queuedBytes, 0)
        XCTAssertTrue(scheduler.snapshot().awaitingKeyframe)
        scheduler.receive(accessUnit: Data(repeating: 1, count: 9), streamID: 1, frameID: 3, version: 1, generation: 1)
        XCTAssertEqual(scheduler.snapshot().queuedFrames, 0)
    }
}
