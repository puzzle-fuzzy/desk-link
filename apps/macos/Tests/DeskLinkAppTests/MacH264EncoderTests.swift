import Foundation
import XCTest
@testable import DeskLinkApp

final class MacH264EncoderTests: XCTestCase {
    func testAssemblerPublishesConfigurationBeforeAnnexBAccessUnit() throws {
        let format = H264EncoderFormat(
            sps: Data([0x67, 0x4d, 0x00, 0x1f]),
            pps: Data([0x68, 0xee, 0x3c, 0x80])
        )
        let avcc = Data([0, 0, 0, 3, 0x65, 0xaa, 0xbb])

        let events = try H264EncoderOutputAssembler.events(
            avccAccessUnit: avcc,
            format: format,
            frameID: 42,
            streamID: 7,
            width: 1920,
            height: 1080,
            configVersion: 3,
            emitsConfiguration: true
        )

        XCTAssertEqual(events.count, 2)
        XCTAssertEqual(events[0], .configuration(
            streamID: 7,
            version: 3,
            width: 1920,
            height: 1080,
            annexBParameterSets: Data([0, 0, 0, 1, 0x67, 0x4d, 0x00, 0x1f, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80])
        ))
        XCTAssertEqual(events[1], .accessUnit(
            streamID: 7,
            frameID: 42,
            configVersion: 3,
            isKeyframe: true,
            annexB: Data([0, 0, 0, 1, 0x65, 0xaa, 0xbb])
        ))
    }
    func testRejectsUndeclaredHighProfileBeforeEmittingConfiguration() {
        XCTAssertThrowsError(try H264EncoderOutputAssembler.events(
            avccAccessUnit: Data([0, 0, 0, 2, 0x65, 0xaa]),
            format: H264EncoderFormat(sps: Data([0x67, 0x64, 0, 31]), pps: Data([0x68, 1])),
            frameID: 1, streamID: 1, width: 1920, height: 1080,
            configVersion: 1, emitsConfiguration: true
        )) { XCTAssertEqual($0 as? MacH264EncoderError, .unsupportedOutputProfile) }
    }

    func testCaptureAndEncoderStayInsideAdvertisedDimensions() {
        let dimensions = captureVideoDimensions(displayWidth: 3840, displayHeight: 2160)
        XCTAssertEqual(dimensions, CaptureVideoDimensions(width: 1920, height: 1080))
        XCTAssertTrue(MacHostVideoContract.supports(width: dimensions.width, height: dimensions.height))
        XCTAssertFalse(MacHostVideoContract.supports(width: 2560, height: 1440))
        XCTAssertFalse(MacHostVideoContract.supports(width: 0, height: 1080))
    }
}
