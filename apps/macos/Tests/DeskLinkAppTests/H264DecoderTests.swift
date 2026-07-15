import XCTest
@testable import DeskLinkApp

@MainActor
final class H264DecoderTests: XCTestCase {
    func testDecoderStartsAndResetsWithoutRetainingStreamState() {
        let decoder = H264Decoder()
        XCTAssertNil(decoder.latestPixelBuffer)
        XCTAssertEqual(decoder.lastFrameID, 0)
        XCTAssertEqual(decoder.configVersion, 0)
        decoder.reset()
        XCTAssertNil(decoder.latestPixelBuffer)
        XCTAssertEqual(decoder.lastFrameID, 0)
        XCTAssertEqual(decoder.configVersion, 0)
    }

    func testAsynchronousDecodeFlagsAreAvailableOnCurrentSDK() {
        XCTAssertNotEqual(H264Decoder.decodeFlags.rawValue, 0)
    }
}
