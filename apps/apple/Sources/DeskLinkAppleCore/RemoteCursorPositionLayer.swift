import SwiftUI

/// A neutral position marker, not a reconstruction of the host cursor shape.
/// v12 only transmits an opaque shape handle, so no hotspot can be inferred.
public struct RemoteCursorPositionLayer: View {
    public let overlay: CursorOverlay?
    public let activeStreamID: UInt64?
    public let videoRect: CGRect
    @State private var cursor = RemoteCursorPositionState()

    public init(overlay: CursorOverlay?, activeStreamID: UInt64?, videoRect: CGRect) {
        self.overlay = overlay
        self.activeStreamID = activeStreamID
        self.videoRect = videoRect
    }

    public var body: some View {
        ZStack(alignment: .topLeading) {
            if cursor.streamID == activeStreamID,
               let point = cursor.latest?.point(in: videoRect) {
                Circle()
                    .fill(Color.black.opacity(0.65))
                    .overlay(Circle().stroke(Color.white, lineWidth: 1.5))
                    .frame(width: 12, height: 12)
                    .position(point)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .allowsHitTesting(false)
        .accessibilityHidden(true)
        .transaction { $0.animation = nil }
        .onAppear { consume() }
        .onChange(of: overlay) { _ in consume() }
        .onChange(of: activeStreamID) { _ in consume() }
    }

    private func consume() {
        cursor.consume(overlay, activeStreamID: activeStreamID)
    }
}
