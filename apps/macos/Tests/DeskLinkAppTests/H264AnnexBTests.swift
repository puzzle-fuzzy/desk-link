import Foundation
import XCTest
@testable import DeskLinkApp

final class H264AnnexBTests: XCTestCase {
    func testExtractsParameterSetsAcrossThreeAndFourByteStartCodes() throws {
        let sequenceHeader = Data([
            0, 0, 0, 1, 0x67, 0x64, 0x00, 0x1f,
            0, 0, 1, 0x68, 0xee, 0x3c, 0x80,
        ])

        let parameterSets = try H264AnnexB.parameterSets(in: sequenceHeader)

        XCTAssertEqual(parameterSets.sps, Data([0x67, 0x64, 0x00, 0x1f]))
        XCTAssertEqual(parameterSets.pps, Data([0x68, 0xee, 0x3c, 0x80]))
    }

    func testConvertsAnnexBAccessUnitToFourByteLengthPrefixedAVCC() throws {
        let accessUnit = Data([
            0, 0, 0, 1, 0x65, 0xaa, 0xbb,
            0, 0, 1, 0x41, 0xcc,
        ])

        XCTAssertEqual(
            try H264AnnexB.avccAccessUnit(from: accessUnit),
            Data([
                0, 0, 0, 3, 0x65, 0xaa, 0xbb,
                0, 0, 0, 2, 0x41, 0xcc,
            ])
        )
    }

    func testRejectsMissingStartCodesAndIncompleteParameterSets() {
        XCTAssertThrowsError(try H264AnnexB.avccAccessUnit(from: Data([0x65, 1, 2]))) {
            XCTAssertEqual($0 as? H264AnnexBError, .noNALUnits)
        }
        XCTAssertThrowsError(
            try H264AnnexB.parameterSets(in: Data([0, 0, 1, 0x67, 1, 2]))
        ) {
            XCTAssertEqual($0 as? H264AnnexBError, .missingParameterSets)
        }
    }
}
