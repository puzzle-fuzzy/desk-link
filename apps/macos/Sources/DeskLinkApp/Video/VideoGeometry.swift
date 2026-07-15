import CoreGraphics

enum VideoGeometry {
    static func aspectFit(source: CGSize, in bounds: CGRect) -> CGRect {
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
}
