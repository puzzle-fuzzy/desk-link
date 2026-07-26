@preconcurrency import CoreVideo
import CoreImage
import DeskLinkAppleCore
@preconcurrency import MetalKit
import SwiftUI

struct IOSMetalVideoView: UIViewRepresentable {
    let pixelBuffer: CVPixelBuffer?
    let videoSize: CGSize?
    let zoomScale: CGFloat
    let panOffset: CGSize
    @Binding var visibleVideoRect: CGRect

    func makeUIView(context: Context) -> IOSMetalVideoSurface {
        let view = IOSMetalVideoSurface()
        view.onVisibleVideoRectChanged = { rect in
            visibleVideoRect = rect
        }
        view.update(
            pixelBuffer: pixelBuffer,
            videoSize: videoSize,
            zoomScale: zoomScale,
            panOffset: panOffset
        )
        return view
    }

    func updateUIView(_ view: IOSMetalVideoSurface, context: Context) {
        view.onVisibleVideoRectChanged = { rect in
            visibleVideoRect = rect
        }
        view.update(
            pixelBuffer: pixelBuffer,
            videoSize: videoSize,
            zoomScale: zoomScale,
            panOffset: panOffset
        )
    }
}

@MainActor
final class IOSMetalVideoSurface: MTKView {
    var onVisibleVideoRectChanged: ((CGRect) -> Void)?

    private let commandQueue: MTLCommandQueue?
    private let ciContext: CIContext?
    private var pixelBuffer: CVPixelBuffer?
    private var videoSize: CGSize?
    private var zoomScale: CGFloat = 1
    private var panOffset: CGSize = .zero
    private var lastPublishedRect: CGRect = .null

    override init(frame frameRect: CGRect, device: MTLDevice?) {
        let metalDevice = device ?? MTLCreateSystemDefaultDevice()
        commandQueue = metalDevice.flatMap { $0.makeCommandQueue() }
        ciContext = metalDevice.map { CIContext(mtlDevice: $0) }
        super.init(frame: frameRect, device: metalDevice)
        framebufferOnly = false
        enableSetNeedsDisplay = true
        isPaused = true
        autoResizeDrawable = true
        colorPixelFormat = .bgra8Unorm
        clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 1)
        backgroundColor = .black
    }

    required init(coder: NSCoder) { fatalError("IOSMetalVideoSurface does not support NSCoder initialization") }

    func update(
        pixelBuffer: CVPixelBuffer?,
        videoSize: CGSize?,
        zoomScale: CGFloat,
        panOffset: CGSize
    ) {
        self.pixelBuffer = pixelBuffer
        self.videoSize = videoSize
        self.zoomScale = zoomScale
        self.panOffset = panOffset
        lastPublishedRect = .null
        publishVisibleVideoRect()
        setNeedsDisplay()
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        publishVisibleVideoRect()
    }

    override func draw(_ rect: CGRect) {
        publishVisibleVideoRect()
        guard let drawable = currentDrawable,
              let commandBuffer = commandQueue?.makeCommandBuffer(),
              let ciContext
        else { return }

        let targetRect = CGRect(origin: .zero, size: drawableSize)
        let output: CIImage
        if let pixelBuffer {
            let image = CIImage(cvPixelBuffer: pixelBuffer)
            let baseRect = VideoGeometry.aspectFit(source: image.extent.size, in: targetRect)
            guard !baseRect.isEmpty, !image.extent.isEmpty else { return }
            let scaleX = bounds.width > 0 ? targetRect.width / bounds.width : 1
            let scaleY = bounds.height > 0 ? targetRect.height / bounds.height : 1
            let renderedRect = iosRenderedVideoRect(
                baseRect: baseRect,
                zoomScale: zoomScale,
                panOffset: CGSize(
                    width: panOffset.width * scaleX,
                    height: panOffset.height * scaleY
                )
            )

            let normalized = image.transformed(
                by: CGAffineTransform(
                    translationX: -image.extent.minX,
                    y: -image.extent.minY
                )
            )
            let scaled = normalized.transformed(
                by: CGAffineTransform(
                    scaleX: renderedRect.width / image.extent.width,
                    y: renderedRect.height / image.extent.height
                )
            )
            let positioned = scaled.transformed(
                by: CGAffineTransform(
                    translationX: renderedRect.minX,
                    y: renderedRect.minY
                )
            )
            output = positioned.composited(
                over: CIImage(color: .black).cropped(to: targetRect)
            )
        } else {
            output = CIImage(color: .black).cropped(to: targetRect)
        }

        ciContext.render(
            output,
            to: drawable.texture,
            commandBuffer: commandBuffer,
            bounds: targetRect,
            colorSpace: CGColorSpaceCreateDeviceRGB()
        )
        commandBuffer.present(drawable)
        commandBuffer.commit()
    }

    private func publishVisibleVideoRect() {
        let source = videoSize ?? pixelBuffer.map {
            CGSize(width: CVPixelBufferGetWidth($0), height: CVPixelBufferGetHeight($0))
        } ?? .zero
        let baseRect = VideoGeometry.aspectFit(source: source, in: bounds)
        let rect = iosRenderedVideoRect(
            baseRect: baseRect,
            zoomScale: zoomScale,
            panOffset: panOffset
        )
        guard rect != lastPublishedRect else { return }
        lastPublishedRect = rect
        onVisibleVideoRectChanged?(rect)
    }
}
