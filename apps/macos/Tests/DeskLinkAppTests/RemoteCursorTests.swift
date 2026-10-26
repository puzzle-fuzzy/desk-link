import DeskLinkAppleCore
import Foundation
import XCTest
@testable import DeskLinkApp

final class RemoteCursorTests: XCTestCase {
    func testProtocolCursorDecodesPositionVisibilityAndOpaqueShape() throws {
        let cursor = try XCTUnwrap(RemoteCursor(overlay: CursorOverlay(streamID: 7, encodedUpdate: packet())))
        XCTAssertEqual(cursor.streamID, 7)
        XCTAssertEqual(cursor.sequence, 300)
        XCTAssertEqual(cursor.normalizedPosition.x, 0.25)
        XCTAssertEqual(cursor.normalizedPosition.y, 0.75)
        XCTAssertTrue(cursor.isVisible)
        XCTAssertEqual(cursor.shapeID, 0x123456789)
        let hidden = try XCTUnwrap(RemoteCursor(overlay: CursorOverlay(streamID: 7, encodedUpdate: packet(visible: 0))))
        XCTAssertFalse(hidden.isVisible)
    }

    func testRejectsMismatchedStreamMalformedVarintTruncationAndOutOfBoundsPosition() {
        XCTAssertNil(RemoteCursor(overlay: CursorOverlay(streamID: 8, encodedUpdate: packet())))
        XCTAssertNil(RemoteCursor(overlay: CursorOverlay(streamID: 7, encodedUpdate: Data(packet().dropLast()))))
        XCTAssertNil(RemoteCursor(overlay: CursorOverlay(streamID: 7, encodedUpdate: packet(x: 2_000_002))))
        XCTAssertNil(RemoteCursor(overlay: CursorOverlay(streamID: 7, encodedUpdate: packet(x: 1))))
        XCTAssertNil(RemoteCursor(overlay: CursorOverlay(streamID: 7, encodedUpdate: packet(visible: 2))))
        XCTAssertNil(RemoteCursor(overlay: CursorOverlay(streamID: 7, encodedUpdate: packet() + Data([0]))))
        XCTAssertNil(RemoteCursor(overlay: CursorOverlay(streamID: 7, encodedUpdate: Data(repeating: 0xff, count: 20))))
    }

    private func packet(x: UInt64 = 500_000, visible: UInt8 = 1) -> Data {
        var bytes: [UInt8] = []
        for number: UInt64 in [12, 7, 300, 1_000_000, x, 1_500_000] { bytes += varint(number) }
        bytes.append(visible)
        bytes += varint(0x123456789)
        return Data(bytes)
    }

    private func varint(_ number: UInt64) -> [UInt8] {
        var remaining = number
        var bytes: [UInt8] = []
        while remaining >= 0x80 {
            bytes.append(UInt8(remaining & 0x7f) | 0x80)
            remaining >>= 7
        }
        bytes.append(UInt8(remaining))
        return bytes
    }
}
