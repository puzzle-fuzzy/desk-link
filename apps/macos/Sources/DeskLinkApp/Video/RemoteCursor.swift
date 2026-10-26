import CoreGraphics
import DeskLinkAppleCore
import Foundation

/// CursorUpdate is the protocol's postcard record, not JSON. Its shape ID is
/// an opaque host OS handle; the current protocol carries no image or hotspot.
/// Show the same generic arrow as the Windows controller until shape images
/// become part of the shared protocol.
struct RemoteCursor: Equatable {
    let streamID: UInt64
    let sequence: UInt64
    let normalizedPosition: CGPoint
    let isVisible: Bool
    let shapeID: UInt64

    init?(overlay: CursorOverlay) {
        guard !overlay.encodedUpdate.isEmpty, overlay.encodedUpdate.count <= 256 else { return nil }
        var reader = CursorPostcardReader(data: overlay.encodedUpdate)
        guard reader.unsigned(maxBits: 16) == 12,
              let streamID = reader.unsigned(maxBits: 64), streamID > 0, streamID == overlay.streamID,
              let sequence = reader.unsigned(maxBits: 64), sequence > 0,
              reader.unsigned(maxBits: 64) != nil,
              let encodedX = reader.unsigned(maxBits: 32),
              let encodedY = reader.unsigned(maxBits: 32),
              // Nonnegative signed postcard integers have an even zigzag form.
              encodedX & 1 == 0, encodedY & 1 == 0,
              encodedX / 2 <= 1_000_000, encodedY / 2 <= 1_000_000,
              let visible = reader.boolean(),
              let shapeID = reader.unsigned(maxBits: 64), reader.isAtEnd
        else { return nil }
        self.streamID = streamID
        self.sequence = sequence
        normalizedPosition = CGPoint(x: Double(encodedX / 2) / 1_000_000,
                                     y: Double(encodedY / 2) / 1_000_000)
        isVisible = visible
        self.shapeID = shapeID
    }
}

private struct CursorPostcardReader {
    let data: Data
    var offset = 0

    var isAtEnd: Bool { offset == data.count }

    mutating func unsigned(maxBits: Int) -> UInt64? {
        var value: UInt64 = 0
        var shift = 0
        while offset < data.count, shift < maxBits {
            let byte = data[data.startIndex + offset]
            offset += 1
            let payload = UInt64(byte & 0x7f)
            let remaining = maxBits - shift
            guard remaining >= 7 || payload < (UInt64(1) << remaining) else { return nil }
            value |= payload << shift
            if byte & 0x80 == 0 { return value }
            shift += 7
        }
        return nil
    }

    mutating func boolean() -> Bool? {
        guard offset < data.count else { return nil }
        let byte = data[data.startIndex + offset]
        offset += 1
        switch byte {
        case 0: return false
        case 1: return true
        default: return nil
        }
    }
}
