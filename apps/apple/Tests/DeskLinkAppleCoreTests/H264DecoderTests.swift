import XCTest
@testable import DeskLinkAppleCore

@MainActor
final class H264DecoderTests: XCTestCase {
    func testDecodedOutputCanTrailSubmissionWithoutFreezingPresentation() {
        var order = DecodedFrameOrder()
        let generation = order.generation
        order.submitted(10)
        order.submitted(11)
        order.submitted(12)

        XCTAssertTrue(order.accept(frameID: 10, generation: generation))
        XCTAssertTrue(order.accept(frameID: 12, generation: generation))
        XCTAssertFalse(order.accept(frameID: 11, generation: generation))
        XCTAssertFalse(order.accept(frameID: 12, generation: generation))
        XCTAssertFalse(order.accept(frameID: 13, generation: generation))
    }

    func testResetRejectsQueuedOutputEvenWhenNewStreamReusesFrameNumbers() {
        var order = DecodedFrameOrder()
        let oldGeneration = order.generation
        order.submitted(20)
        order.reset()
        order.submitted(20)

        XCTAssertFalse(order.accept(frameID: 20, generation: oldGeneration))
        XCTAssertTrue(order.accept(frameID: 20, generation: order.generation))
        order.reset()
        XCTAssertFalse(order.accept(frameID: 20, generation: order.generation))
    }

    func testDecoderStartsAndResetsWithoutRetainingStreamState() {
        let decoder = H264Decoder()
        XCTAssertNil(decoder.latestPixelBuffer)
        XCTAssertEqual(decoder.lastFrameID, 0)
        XCTAssertEqual(decoder.configVersion, 0)

        let sequenceHeader = Data([
            0, 0, 0, 1, 0x67, 0x64, 0x00, 0x1f, 0xac, 0xd9, 0x40, 0x50,
            0x05, 0xbb, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00,
            0x00, 0x03, 0x03, 0x20, 0xf1, 0x22, 0x6a,
            0, 0, 1, 0x68, 0xee, 0x3c, 0x80,
        ])
        XCTAssertTrue(decoder.configure(sequenceHeader: sequenceHeader, width: 1920, height: 1080, version: 1))
        XCTAssertEqual(decoder.configVersion, 1)

        decoder.reset()
        XCTAssertNil(decoder.latestPixelBuffer)
        XCTAssertEqual(decoder.lastFrameID, 0)
        XCTAssertEqual(decoder.configVersion, 0)
        XCTAssertEqual(decoder.configuredWidth, 0)
        XCTAssertEqual(decoder.configuredHeight, 0)
    }

    func testAsynchronousDecodeFlagsAreAvailableOnCurrentSDK() {
        XCTAssertNotEqual(H264Decoder.decodeFlags.rawValue, 0)
    }
}
