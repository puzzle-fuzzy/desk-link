@preconcurrency import CoreVideo
import CoreImage
import DeskLinkAppleCore
@preconcurrency import MetalKit
import SwiftUI

struct IOSMetalVideoView: UIViewRepresentable {
    let frame: PresentedVideoFrame?
    let onFramePresented: @MainActor @Sendable (VideoFramePresentationToken, CGRect) -> Bool
    let zoomScale: CGFloat
    let panOffset: CGSize
    @Binding var visibleVideoRect: CGRect

    func makeUIView(context: Context) -> IOSMetalVideoSurface {
        let view = IOSMetalVideoSurface()
        view.onVisibleVideoRectChanged = { rect in
            visibleVideoRect = rect
        }
        view.onFramePresented = onFramePresented
        view.update(
            frame: frame,
            zoomScale: zoomScale,
            panOffset: panOffset
        )
        return view
    }

    func updateUIView(_ view: IOSMetalVideoSurface, context: Context) {
        view.onVisibleVideoRectChanged = { rect in
            visibleVideoRect = rect
        }
        view.onFramePresented = onFramePresented
        view.update(
            frame: frame,
            zoomScale: zoomScale,
            panOffset: panOffset
        )
    }
}

@MainActor
final class IOSMetalVideoSurface: MTKView {
    var onVisibleVideoRectChanged: ((CGRect) -> Void)?
    var onFramePresented: (@MainActor @Sendable (VideoFramePresentationToken, CGRect) -> Bool)?

    private let commandQueue: MTLCommandQueue?
    private let ciContext: CIContext?
    private var currentFrame: PresentedVideoFrame?
    private var pixelBuffer: CVPixelBuffer? { currentFrame?.pixelBuffer }
    private var presentationToken: VideoFramePresentationToken? { currentFrame?.token }
    private var presentedVideoRect: CGRect = .zero
    private var lastPresentedTime: CFTimeInterval = 0
    private var zoomScale: CGFloat = 1
    private var panOffset: CGSize = .zero
    private var lastPublishedRect: CGRect = .null
    private var lastLayoutBounds: CGRect = .null

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
        frame: PresentedVideoFrame?,
        zoomScale: CGFloat,
        panOffset: CGSize
    ) {
        let pixelBuffer = frame?.pixelBuffer
        guard self.pixelBuffer !== pixelBuffer || self.presentationToken != frame?.token
                || self.zoomScale != zoomScale || self.panOffset != panOffset
        else { return }
        currentFrame = frame
        self.zoomScale = zoomScale
        self.panOffset = panOffset
        lastPublishedRect = .null
        publishVisibleVideoRect()
        setNeedsDisplay()
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        guard bounds != lastLayoutBounds else { return }
        lastLayoutBounds = bounds
        publishVisibleVideoRect()
        setNeedsDisplay()
    }

    override func draw(_ rect: CGRect) {
        publishVisibleVideoRect()
        guard let drawable = currentDrawable,
              let commandBuffer = commandQueue?.makeCommandBuffer(),
              let ciContext
        else { return }

        // Freeze pixels and identity together for this drawable. Neither a
        // later decoded frame nor the previous presented size can affect its
        // rendered geometry or the receipt captured below.
        let frame = currentFrame
        let pixelBuffer = frame?.pixelBuffer
        let token = frame?.token
        let targetRect = CGRect(origin: .zero, size: drawableSize)
        var visibleRect: CGRect = .zero
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

            visibleRect = CGRect(x: renderedRect.minX / scaleX, y: renderedRect.minY / scaleY,
                                 width: renderedRect.width / scaleX, height: renderedRect.height / scaleY)
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
#if !targetEnvironment(simulator)
        // CAMetalDrawable presentation handlers are unavailable in the
        // simulator; its tests assert against presentation fixtures instead.
        if let token, let receipt = onFramePresented, pixelBuffer != nil, !visibleRect.isEmpty {
            let presentedRect = visibleRect
            drawable.addPresentedHandler { [weak self] drawable in
                let presentedTime = drawable.presentedTime
                guard presentedTime > 0 else { return }
                Task { @MainActor [weak self] in
                    guard let self, presentedTime >= self.lastPresentedTime,
                          self.presentationToken?.context == token.context,
                          receipt(token, presentedRect) else { return }
                    self.lastPresentedTime = presentedTime
                    self.presentedVideoRect = presentedRect
                    self.publishVisibleVideoRect()
                }
            }
        }
#endif
        commandBuffer.present(drawable)
        commandBuffer.commit()
    }

    private func publishVisibleVideoRect() {
        // update/layout may run before GPU presentation. Keep the last actual
        // presented geometry until the drawable receipt replaces it.
        guard presentedVideoRect != lastPublishedRect else { return }
        lastPublishedRect = presentedVideoRect
        onVisibleVideoRectChanged?(presentedVideoRect)
    }
}
