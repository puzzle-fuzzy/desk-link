import XCTest
@testable import DeskLinkApp

@MainActor
final class H264DecoderTests: XCTestCase {
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
