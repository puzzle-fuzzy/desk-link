import CoreGraphics

public enum VideoGeometry {
    public static func aspectFit(source: CGSize, in bounds: CGRect) -> CGRect {
        guard source.width > 0,
              source.height > 0,
              bounds.width > 0,
              bounds.height > 0
        else { return .zero }
        let scale = min(bounds.width / source.width, bounds.height / source.height)
        let size = CGSize(width: source.width * scale, height: source.height * scale)
        return CGRect(
            x: bounds.midX - size.width / 2,
            y: bounds.midY - size.height / 2,
            width: size.width,
            height: size.height
        )
    }

    /// Converts a point from a top-left-origin video rect to the normalized
    /// coordinates used by the remote input protocol.
    ///
    /// Both UIKit and the remote display protocol use this convention. Other
    /// platform-specific coordinate systems must be converted at their input
    /// boundary before calling this helper.
    public static func normalizedTopLeftPoint(for point: CGPoint, in rect: CGRect) -> CGPoint? {
        guard rect.width > 0,
              rect.height > 0,
              point.x >= rect.minX,
              point.x <= rect.maxX,
              point.y >= rect.minY,
              point.y <= rect.maxY
        else { return nil }

        return CGPoint(
            x: (point.x - rect.minX) / rect.width,
            y: (point.y - rect.minY) / rect.height
        )
    }
}
