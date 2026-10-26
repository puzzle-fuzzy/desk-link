import Foundation
import XCTest
@testable import DeskLinkAppleCore

final class SessionControlStateTests: XCTestCase {
    // The protocol crate verifies every byte with encode_control/decode_control.
    private let displaysFixture: [UInt8] = [4, 2, 7, 128, 15, 184, 8, 1, 172, 2, 128, 20, 160, 11, 0, 172, 2]
    private let automaticQualityFixture: [UInt8] = [10, 0, 1]
    private let sharpQualityFixture: [UInt8] = [10, 3, 2]
    private let audioFixture: [UInt8] = [7, 1, 0]
    private let inputBlockedFixture: [UInt8] = [14, 1, 0]
    private let inputRecoveredFixture: [UInt8] = [14, 0]

    func testDisplayListAndActiveSelectionDecodeProductionBytes() throws {
        guard case let .displays(displays, activeID) = try XCTUnwrap(SessionControlState(encoded: Data(displaysFixture))) else {
            return XCTFail("display state required")
        }
        XCTAssertEqual(displays.count, 2)
        XCTAssertEqual(displays.map(\.id), [7, 300])
        XCTAssertEqual(displays.map(\.width), [1920, 2560])
        XCTAssertEqual(displays.map(\.height), [1080, 1440])
        XCTAssertEqual(displays.map(\.primary), [true, false])
        XCTAssertEqual(activeID, 300)
    }

    func testQualityPreferenceAndEffectivePresetRemainSeparate() {
        XCTAssertEqual(SessionControlState(encoded: Data(automaticQualityFixture)), .videoQuality(SessionVideoQuality(preference: .automatic, preset: .balanced)))
        XCTAssertEqual(SessionControlState(encoded: Data(sharpQualityFixture)), .videoQuality(SessionVideoQuality(preference: .sharp, preset: .sharp)))
        XCTAssertEqual(SessionControlState(encoded: Data(audioFixture)), .audio(available: true, enabled: false))
    }

    func testInputRestrictionRecoversWithoutConnectionFailure() {
        XCTAssertEqual(SessionControlState(encoded: Data(inputBlockedFixture)), .inputRestriction(.blocked))
        XCTAssertEqual(SessionControlState(encoded: Data(inputRecoveredFixture)), .inputRestriction(nil))
    }

    func testAllTruncationAndTrailingBytesAreRejected() {
        for fixture in [displaysFixture, automaticQualityFixture, sharpQualityFixture, audioFixture, inputBlockedFixture, inputRecoveredFixture] {
            for length in 0..<fixture.count { XCTAssertNil(SessionControlState(encoded: Data(fixture.prefix(length)))) }
            XCTAssertNil(SessionControlState(encoded: Data(fixture + [0])))
        }
    }

    func testMalformedCountDimensionsIDEnumsAndCanonicalVarintsAreRejected() {
        let invalid: [[UInt8]] = [
            [4, 0, 0], [4, 17], [4, 1, 1, 0, 1, 1, 1],
            [4, 2, 1, 1, 1, 0, 1, 1, 1, 0, 1],
            [4, 1, 1, 1, 1, 0, 2],
            [4, 1, 255, 255, 255, 255, 31],
            [4, 1, 0, 128, 128, 4],
            [132, 0, 1, 1, 1, 1, 0, 1],
            [7, 2, 0], [10, 4, 1], [10, 0, 3], [14, 2], [14, 1, 3],
        ]
        for fixture in invalid { XCTAssertNil(SessionControlState(encoded: Data(fixture))) }
        XCTAssertNil(SessionControlState(encoded: Data(repeating: 0, count: 257)))
    }
}
