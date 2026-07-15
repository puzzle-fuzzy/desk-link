import Foundation
import XCTest
@testable import DeskLinkApp

final class MacH264EncoderTests: XCTestCase {
    func testAssemblerPublishesConfigurationBeforeAnnexBAccessUnit() throws {
        let format = H264EncoderFormat(
            sps: Data([0x67, 0x64, 0x00, 0x1f]),
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
            annexBParameterSets: Data([0, 0, 0, 1, 0x67, 0x64, 0x00, 0x1f, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80])
        ))
        XCTAssertEqual(events[1], .accessUnit(
            streamID: 7,
            frameID: 42,
            configVersion: 3,
            isKeyframe: true,
            annexB: Data([0, 0, 0, 1, 0x65, 0xaa, 0xbb])
        ))
    }
}
