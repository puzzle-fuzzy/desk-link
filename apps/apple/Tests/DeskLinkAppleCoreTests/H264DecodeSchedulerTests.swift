import Foundation
import XCTest
@testable import DeskLinkAppleCore

final class H264DecodeSchedulerTests: XCTestCase {
    private func makeScheduler(queue: DispatchQueue, limits: H264DecodeScheduler.Limits) -> H264DecodeScheduler {
        H264DecodeScheduler(limits: limits, decodeQueue: queue, onFrame: { _ in },
                            onConfiguration: { _, _, _, _, _, _ in }, onKeyframeRequest: { _, _ in })
    }

    func testCongestionDropsWholeDependentChainAndWaitsForIDR() {
        let queue = DispatchQueue(label: "test.decoder.paused")
        queue.suspend()
        defer { queue.resume() }
        let scheduler = makeScheduler(queue: queue, limits: .init(frames: 2, bytes: 64))
        scheduler.reset(generation: 7)
        scheduler.configure(sequenceHeader: Data([1]), streamID: 2, inputEpoch: 1, width: 10, height: 10, version: 1, generation: 7)
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
        scheduler.configure(sequenceHeader: Data([1]), streamID: 1, inputEpoch: 1, width: 10, height: 10, version: 1, generation: 7)
        XCTAssertFalse(scheduler.accepts(generation: 7, streamID: 1, version: 1, inputEpoch: 1))
        scheduler.configure(sequenceHeader: Data([1]), streamID: 2, inputEpoch: 1, width: 10, height: 10, version: 2, generation: 8)
        scheduler.receive(accessUnit: Data([0, 0, 1, 0x65, 1]), streamID: 2, frameID: 1, version: 1, generation: 8)
        XCTAssertEqual(scheduler.snapshot().queuedFrames, 0)
        XCTAssertTrue(scheduler.accepts(generation: 8, streamID: 2, version: 2, inputEpoch: 1))
        scheduler.configure(sequenceHeader: Data([1]), streamID: 3, inputEpoch: 1, width: 10, height: 10, version: 1, generation: 8)
        XCTAssertFalse(scheduler.accepts(generation: 8, streamID: 2, version: 2, inputEpoch: 1))
        XCTAssertTrue(scheduler.accepts(generation: 8, streamID: 3, version: 1, inputEpoch: 1))
    }

    func testInputEpochIsOpaqueAndOnlyAConfigurationVersionCanChangeIt() {
        let queue = DispatchQueue(label: "test.decoder.input-epoch")
        queue.suspend()
        defer { queue.resume() }
        let scheduler = makeScheduler(queue: queue, limits: .init())
        scheduler.reset(generation: 9)
        scheduler.configure(sequenceHeader: Data([1]), streamID: 4, inputEpoch: 20, width: 10, height: 10, version: 1, generation: 9)
        XCTAssertTrue(scheduler.accepts(generation: 9, streamID: 4, version: 1, inputEpoch: 20))
        scheduler.configure(sequenceHeader: Data([1]), streamID: 4, inputEpoch: 3, width: 10, height: 10, version: 1, generation: 9)
        XCTAssertFalse(scheduler.accepts(generation: 9, streamID: 4, version: 1, inputEpoch: 3))
        scheduler.configure(sequenceHeader: Data([1]), streamID: 4, inputEpoch: 3, width: 10, height: 10, version: 2, generation: 9)
        XCTAssertFalse(scheduler.acceptsInputSurface(generation: 9, streamID: 4, inputEpoch: 20))
        XCTAssertTrue(scheduler.accepts(generation: 9, streamID: 4, version: 2, inputEpoch: 3))
    }

    func testSameHandleReconnectAdmitsSmallerStreamEpochAndVersionOne() {
        let queue = DispatchQueue(label: "test.decoder.same-handle-reconnect")
        queue.suspend()
        defer { queue.resume() }
        let scheduler = makeScheduler(queue: queue, limits: .init())
        scheduler.reset(generation: 9)
        scheduler.configure(sequenceHeader: Data([1]), streamID: 20, inputEpoch: 100, width: 10, height: 10, version: 8, generation: 9)
        scheduler.receive(accessUnit: Data([0, 0, 1, 0x65, 1]), streamID: 20, frameID: 50, version: 8, generation: 9)
        XCTAssertEqual(scheduler.snapshot().queuedFrames, 1)
        XCTAssertTrue(scheduler.retireStream(generation: 9))
        XCTAssertEqual(scheduler.snapshot().queuedFrames, 0)
        XCTAssertFalse(scheduler.acceptsInputSurface(generation: 9, streamID: 20, inputEpoch: 100))
        scheduler.configure(sequenceHeader: Data([1]), streamID: 1, inputEpoch: 2, width: 20, height: 10, version: 1, generation: 9)
        XCTAssertTrue(scheduler.accepts(generation: 9, streamID: 1, version: 1, inputEpoch: 2))
        XCTAssertFalse(scheduler.accepts(generation: 9, streamID: 20, version: 8, inputEpoch: 100))
        scheduler.receive(accessUnit: Data([0, 0, 1, 0x65, 1]), streamID: 1, frameID: 1, version: 1, generation: 9)
        XCTAssertEqual(scheduler.snapshot().queuedFrames, 1)
    }

    func testRetiredCallbackCannotResetSuccessorRuntimeConfiguration() {
        let queue = DispatchQueue(label: "test.decoder.retired-callback")
        queue.suspend()
        defer { queue.resume() }
        let scheduler = makeScheduler(queue: queue, limits: .init())
        scheduler.reset(generation: 10)
        scheduler.configure(sequenceHeader: Data([1]), streamID: 1, inputEpoch: 2, width: 10, height: 10, version: 1, generation: 10)
        XCTAssertFalse(scheduler.retireStream(generation: 9))
        XCTAssertTrue(scheduler.accepts(generation: 10, streamID: 1, version: 1, inputEpoch: 2))
    }

    func testSameEpochQualityVersionKeepsSurfaceButDimensionChangeFailsClosed() {
        let queue = DispatchQueue(label: "test.decoder.quality-surface")
        queue.suspend()
        defer { queue.resume() }
        let scheduler = makeScheduler(queue: queue, limits: .init())
        scheduler.reset(generation: 9)
        scheduler.configure(sequenceHeader: Data([1]), streamID: 1, inputEpoch: 2, width: 10, height: 10, version: 1, generation: 9)
        scheduler.configure(sequenceHeader: Data([1]), streamID: 1, inputEpoch: 2, width: 10, height: 10, version: 2, generation: 9)
        XCTAssertFalse(scheduler.accepts(generation: 9, streamID: 1, version: 1, inputEpoch: 2))
        XCTAssertTrue(scheduler.acceptsInputSurface(generation: 9, streamID: 1, inputEpoch: 2))
        scheduler.configure(sequenceHeader: Data([1]), streamID: 1, inputEpoch: 2, width: 20, height: 10, version: 3, generation: 9)
        XCTAssertNil(scheduler.currentInputContext(generation: 9))
        XCTAssertFalse(scheduler.acceptsInputSurface(generation: 9, streamID: 1, inputEpoch: 2))
        scheduler.configure(sequenceHeader: Data([1]), streamID: 1, inputEpoch: 2, width: 10, height: 10, version: 4, generation: 9)
        XCTAssertNil(scheduler.currentInputContext(generation: 9))
        XCTAssertTrue(scheduler.retireStream(generation: 9))
        scheduler.configure(sequenceHeader: Data([1]), streamID: 1, inputEpoch: 1, width: 20, height: 10, version: 1, generation: 9)
        XCTAssertTrue(scheduler.acceptsInputSurface(generation: 9, streamID: 1, inputEpoch: 1))
    }

    func testByteBudgetAndAgeInvalidateReferenceChain() {
        let queue = DispatchQueue(label: "test.decoder.age")
        queue.suspend()
        defer { queue.resume() }
        let scheduler = makeScheduler(queue: queue, limits: .init(frames: 6, bytes: 8, ageNanoseconds: 1))
        scheduler.reset(generation: 1)
        scheduler.configure(sequenceHeader: Data([1]), streamID: 1, inputEpoch: 1, width: 10, height: 10, version: 1, generation: 1)
        scheduler.receive(accessUnit: Data([0, 0, 1, 0x65, 1]), streamID: 1, frameID: 1, version: 1, generation: 1)
        scheduler.receive(accessUnit: Data([0, 0, 1, 0x41, 1]), streamID: 1, frameID: 2, version: 1, generation: 1)
        XCTAssertEqual(scheduler.snapshot().queuedBytes, 0)
        XCTAssertTrue(scheduler.snapshot().awaitingKeyframe)
        scheduler.receive(accessUnit: Data(repeating: 1, count: 9), streamID: 1, frameID: 3, version: 1, generation: 1)
        XCTAssertEqual(scheduler.snapshot().queuedFrames, 0)
    }
}
