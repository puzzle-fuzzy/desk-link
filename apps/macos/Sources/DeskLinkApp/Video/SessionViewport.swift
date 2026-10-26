import CoreGraphics
import DeskLinkAppleCore

enum SessionScalingMode: String, CaseIterable, Identifiable {
    case fit
    case actualSize

    var id: Self { self }
    var title: String { self == .fit ? "适应窗口" : "1:1 原始像素" }
}

enum SessionInteractionMode: String, CaseIterable, Identifiable {
    case control
    case browse

    var id: Self { self }
    var title: String { self == .control ? "远程控制" : "浏览画面" }
}

/// The renderer, cursor and input surface all use this top-left, point-based
/// rectangle. Only the Metal boundary converts it to drawable pixels.
struct SessionViewport: Equatable {
    let sourceSize: CGSize
    let viewportSize: CGSize
    let backingScale: CGFloat
    let scalingMode: SessionScalingMode
    let panOffset: CGSize

    init(
        sourceSize: CGSize,
        viewportSize: CGSize,
        backingScale: CGFloat = 1,
        scalingMode: SessionScalingMode = .fit,
        panOffset: CGSize = .zero
    ) {
        self.sourceSize = sourceSize
        self.viewportSize = viewportSize
        self.backingScale = backingScale.isFinite && backingScale > 0 ? backingScale : 1
        self.scalingMode = scalingMode
        self.panOffset = panOffset
    }

    var videoRect: CGRect {
        guard sourceSize.width.isFinite, sourceSize.height.isFinite,
              viewportSize.width.isFinite, viewportSize.height.isFinite,
              sourceSize.width > 0, sourceSize.height > 0,
              viewportSize.width > 0, viewportSize.height > 0
        else { return .zero }
        if scalingMode == .fit {
            return VideoGeometry.aspectFit(source: sourceSize, in: CGRect(origin: .zero, size: viewportSize))
        }
        // One remote pixel occupies one physical display pixel, including on
        // Retina screens where one AppKit point spans multiple physical pixels.
        let size = CGSize(width: sourceSize.width / backingScale, height: sourceSize.height / backingScale)
        let pan = clampedPanOffset
        return CGRect(
            x: (viewportSize.width - size.width) / 2 + pan.width,
            y: (viewportSize.height - size.height) / 2 + pan.height,
            width: size.width,
            height: size.height
        )
    }

    var clampedPanOffset: CGSize {
        guard scalingMode == .actualSize else { return .zero }
        let limitX = max(0, (sourceSize.width / backingScale - viewportSize.width) / 2)
        let limitY = max(0, (sourceSize.height / backingScale - viewportSize.height) / 2)
        return CGSize(
            width: panOffset.width.isFinite ? min(limitX, max(-limitX, panOffset.width)) : 0,
            height: panOffset.height.isFinite ? min(limitY, max(-limitY, panOffset.height)) : 0
        )
    }

    func normalizedPoint(for point: CGPoint) -> CGPoint? {
        guard CGRect(origin: .zero, size: viewportSize).contains(point) else { return nil }
        return VideoGeometry.normalizedTopLeftPoint(for: point, in: videoRect)
    }

    func cursorPoint(normalized: CGPoint) -> CGPoint {
        CGPoint(x: videoRect.minX + normalized.x * videoRect.width,
                y: videoRect.minY + normalized.y * videoRect.height)
    }

    func drawableVideoRect(drawableSize: CGSize) -> CGRect {
        guard viewportSize.width > 0, viewportSize.height > 0 else { return .zero }
        let scaleX = drawableSize.width / viewportSize.width
        let scaleY = drawableSize.height / viewportSize.height
        let rect = videoRect
        return CGRect(x: rect.minX * scaleX,
                      y: (viewportSize.height - rect.maxY) * scaleY,
                      width: rect.width * scaleX,
                      height: rect.height * scaleY)
    }

    func panned(by delta: CGSize) -> CGSize {
        SessionViewport(sourceSize: sourceSize, viewportSize: viewportSize,
                        backingScale: backingScale, scalingMode: scalingMode,
                        panOffset: CGSize(width: clampedPanOffset.width + delta.width,
                                          height: clampedPanOffset.height + delta.height)).clampedPanOffset
    }
}
