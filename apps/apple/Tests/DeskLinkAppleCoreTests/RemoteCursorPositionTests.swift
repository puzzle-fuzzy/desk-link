import CoreGraphics
import Foundation
import XCTest
@testable import DeskLinkAppleCore

final class RemoteCursorPositionTests: XCTestCase {
    // Rust's production cursor codec verifies these exact Swift fixtures.
    private let visibleFixture: [UInt8] = [14, 7, 11, 42, 160, 194, 30, 224, 198, 91, 1, 99]
    private let hiddenFixture: [UInt8] = [14, 7, 12, 43, 0, 128, 137, 122, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 1]
    private let cornerFixture: [UInt8] = [14, 8, 1, 0, 128, 137, 122, 0, 1, 0]

    func testProductionPostcardCursorFixture() throws {
        let position = try XCTUnwrap(RemoteCursorPosition.decode(Data(visibleFixture)))
        XCTAssertEqual(position.streamID, 7)
        XCTAssertEqual(position.sequence, 11)
        XCTAssertEqual(position.timestampMicroseconds, 42)
        XCTAssertEqual(position.xMillionths, 250_000)
        XCTAssertEqual(position.yMillionths, 750_000)
        XCTAssertTrue(position.visible)
        XCTAssertEqual(position.shapeID, 99)
        XCTAssertEqual(position.point(in: CGRect(x: 100, y: 50, width: 800, height: 450)), CGPoint(x: 300, y: 387.5))
    }

    func testHiddenAndOpaqueMaximumShapeHandle() throws {
        let hidden = try XCTUnwrap(RemoteCursorPosition.decode(Data(hiddenFixture)))
        XCTAssertFalse(hidden.visible)
        XCTAssertEqual(hidden.shapeID, UInt64.max)
        XCTAssertNil(hidden.point(in: CGRect(x: 0, y: 0, width: 1920, height: 1080)))
    }

    func testMalformedOrUnsupportedCursorIsRejected() {
        for length in 0..<visibleFixture.count {
            XCTAssertNil(RemoteCursorPosition.decode(Data(visibleFixture.prefix(length))))
        }
        XCTAssertNil(RemoteCursorPosition.decode(Data(visibleFixture + [0])))
        XCTAssertNil(RemoteCursorPosition.decode(Data(repeating: 0, count: 257)))
        var wrongVersion = visibleFixture
        wrongVersion[0] = 12
        XCTAssertNil(RemoteCursorPosition.decode(Data(wrongVersion)))
        var invalidBool = visibleFixture
        invalidBool[10] = 2
        XCTAssertNil(RemoteCursorPosition.decode(Data(invalidBool)))
        var overflowingHandle = hiddenFixture
        overflowingHandle[overflowingHandle.count - 1] = 2
        XCTAssertNil(RemoteCursorPosition.decode(Data(overflowingHandle)))
    }

    func testDatagramReorderingAndRetiredStreamsCannotResurrectMarker() {
        var state = RemoteCursorPositionState()
        let visible = CursorOverlay(streamID: 7, encodedUpdate: Data(visibleFixture))
        let hidden = CursorOverlay(streamID: 7, encodedUpdate: Data(hiddenFixture))
        state.consume(visible, activeStreamID: 7)
        XCTAssertEqual(state.latest?.sequence, 11)
        state.consume(hidden, activeStreamID: 7)
        state.consume(visible, activeStreamID: 7)
        XCTAssertEqual(state.latest?.sequence, 12)
        XCTAssertEqual(state.latest?.visible, false)
        state.consume(visible, activeStreamID: 8)
        XCTAssertNil(state.latest)
        state.consume(CursorOverlay(streamID: 8, encodedUpdate: Data(cornerFixture)), activeStreamID: 8)
        XCTAssertEqual(state.latest?.sequence, 1)
        state.consume(nil, activeStreamID: nil)
        XCTAssertNil(state.latest)
        state.consume(visible, activeStreamID: nil)
        XCTAssertNil(state.latest)
    }

    func testPositionUsesVideoPointSpaceAfterLetterboxZoomAndPan() throws {
        let corner = try XCTUnwrap(RemoteCursorPosition.decode(Data(cornerFixture)))
        let fitted = VideoGeometry.aspectFit(
            source: CGSize(width: 1920, height: 1080),
            in: CGRect(x: 0, y: 0, width: 800, height: 600)
        )
        XCTAssertEqual(corner.point(in: fitted), CGPoint(x: 800, y: 75))
        XCTAssertEqual(corner.point(in: CGRect(x: -80, y: -140, width: 1600, height: 900)), CGPoint(x: 1520, y: -140))
        XCTAssertNil(corner.point(in: .zero))
    }
}
