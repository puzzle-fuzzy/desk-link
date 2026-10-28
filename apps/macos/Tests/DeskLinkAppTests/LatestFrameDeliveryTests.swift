import CoreVideo
import XCTest
@testable import DeskLinkApp

/// Keyframe recovery on a static desktop depends on the delivery layer being
/// able to re-send the most recent captured frame; these tests pin that
/// contract without touching ScreenCaptureKit or the encoder.
final class LatestFrameDeliveryTests: XCTestCase {
    private func makeBuffer() -> CVPixelBuffer {
        var buffer: CVPixelBuffer?
        let status = CVPixelBufferCreate(
            kCFAllocatorDefault,
            4,
            4,
            kCVPixelFormatType_32BGRA,
            nil,
            &buffer
        )
        precondition(status == kCVReturnSuccess)
        return buffer!
    }

    func testReissueDeliversTheMostRecentFrameWithAFreshFrameID() {
        let delivery = LatestFrameDelivery()
        let token = ObjectIdentifier(NSObject())
        let first = expectation(description: "captured frame delivered")
        let reissued = expectation(description: "re-issued frame delivered")
        let lock = NSLock()
        var deliveredFrameIDs: [UInt64] = []
        delivery.begin(stream: token) { _, frameID in
            lock.lock()
            deliveredFrameIDs.append(frameID)
            let count = deliveredFrameIDs.count
            lock.unlock()
            if count == 1 {
                first.fulfill()
            } else if count == 2 {
                reissued.fulfill()
            }
        }
        delivery.submit(stream: token, buffer: makeBuffer())
        wait(for: [first], timeout: 2)
        delivery.reissueLastFrame(stream: token)
        wait(for: [reissued], timeout: 2)
        lock.lock()
        XCTAssertEqual(deliveredFrameIDs, [1, 2])
        lock.unlock()
    }

    func testReissueWithoutACapturedFrameDeliversNothing() {
        let delivery = LatestFrameDelivery()
        let token = ObjectIdentifier(NSObject())
        let none = expectation(description: "nothing delivered")
        none.isInverted = true
        delivery.begin(stream: token) { _, _ in
            none.fulfill()
        }
        delivery.reissueLastFrame(stream: token)
        wait(for: [none], timeout: 0.3)
    }

    func testReissueAfterEndOrForAnotherStreamDeliversNothing() {
        let delivery = LatestFrameDelivery()
        let token = ObjectIdentifier(NSObject())
        let first = expectation(description: "captured frame delivered")
        let none = expectation(description: "nothing delivered after end")
        none.isInverted = true
        delivery.begin(stream: token) { _, _ in first.fulfill() }
        delivery.submit(stream: token, buffer: makeBuffer())
        wait(for: [first], timeout: 2)

        delivery.reissueLastFrame(stream: ObjectIdentifier(NSObject()))
        delivery.end()
        delivery.reissueLastFrame()
        wait(for: [none], timeout: 0.3)
    }
}
