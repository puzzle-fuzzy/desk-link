import CoreGraphics
import Foundation

/// Position metadata carried by protocol v13. shapeID is an opaque host handle;
/// this protocol does not carry bitmap, dimensions, or hotspot information.
public struct RemoteCursorPosition: Equatable, Sendable {
    public static let wireProtocolVersion: UInt16 = deskLinkWireProtocolVersion
    public let streamID: UInt64
    public let sequence: UInt64
    public let timestampMicroseconds: UInt64
    public let xMillionths: Int32
    public let yMillionths: Int32
    public let visible: Bool
    public let shapeID: UInt64

    public static func decode(_ encoded: Data) -> Self? {
        guard !encoded.isEmpty, encoded.count <= 256 else { return nil }
        var reader = CursorPostcardReader(bytes: Array(encoded))
        guard reader.unsigned(bits: 16) == UInt64(wireProtocolVersion),
              let streamID = reader.unsigned(bits: 64), streamID > 0,
              let sequence = reader.unsigned(bits: 64), sequence > 0,
              let timestamp = reader.unsigned(bits: 64),
              let x = reader.signed32(), (0...1_000_000).contains(x),
              let y = reader.signed32(), (0...1_000_000).contains(y),
              let visible = reader.unsigned(bits: 1),
              let shapeID = reader.unsigned(bits: 64), reader.isAtEnd
        else { return nil }
        return Self(
            streamID: streamID, sequence: sequence, timestampMicroseconds: timestamp,
            xMillionths: x, yMillionths: y, visible: visible == 1, shapeID: shapeID
        )
    }

    /// Maps the remote top-left position into the same point-space rectangle
    /// used for the video and input surfaces, including letterbox, zoom and pan.
    public func point(in videoRect: CGRect) -> CGPoint? {
        guard visible, !videoRect.isEmpty,
              videoRect.origin.x.isFinite, videoRect.origin.y.isFinite,
              videoRect.width.isFinite, videoRect.height.isFinite
        else { return nil }
        return CGPoint(
            x: videoRect.minX + CGFloat(xMillionths) / 1_000_000 * videoRect.width,
            y: videoRect.minY + CGFloat(yMillionths) / 1_000_000 * videoRect.height
        )
    }
}

/// Keeps datagram reordering and a retired stream from resurrecting its marker.
public struct RemoteCursorPositionState: Equatable, Sendable {
    public private(set) var streamID: UInt64?
    public private(set) var latest: RemoteCursorPosition?

    public init() {}

    public mutating func consume(_ overlay: CursorOverlay?, activeStreamID: UInt64?) {
        if streamID != activeStreamID || overlay == nil {
            streamID = activeStreamID
            latest = nil
        }
        guard let activeStreamID, activeStreamID > 0, let overlay,
              overlay.streamID == activeStreamID,
              let update = RemoteCursorPosition.decode(overlay.encodedUpdate),
              update.streamID == activeStreamID,
              update.sequence > (latest?.sequence ?? 0)
        else { return }
        latest = update
    }
}

private struct CursorPostcardReader {
    let bytes: [UInt8]
    private var offset = 0
    var isAtEnd: Bool { offset == bytes.count }

    init(bytes: [UInt8]) { self.bytes = bytes }

    mutating func unsigned(bits: Int) -> UInt64? {
        var value: UInt64 = 0
        for index in 0..<((bits + 6) / 7) {
            guard offset < bytes.count else { return nil }
            let byte = bytes[offset]
            offset += 1
            let payload = UInt64(byte & 0x7f)
            let shift = index * 7
            let remaining = min(7, bits - shift)
            guard payload < (UInt64(1) << remaining) else { return nil }
            value |= payload << shift
            if byte & 0x80 == 0 {
                guard index == 0 || payload != 0 else { return nil }
                return value
            }
        }
        return nil
    }

    mutating func signed32() -> Int32? {
        guard let raw = unsigned(bits: 32) else { return nil }
        return Int32(exactly: Int64(raw >> 1) ^ -Int64(raw & 1))
    }
}
