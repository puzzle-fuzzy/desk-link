@preconcurrency import CoreVideo
import CoreImage
import DeskLinkAppleCore
@preconcurrency import MetalKit
import SwiftUI

struct IOSMetalVideoView: UIViewRepresentable {
    let pixelBuffer: CVPixelBuffer?
    let videoSize: CGSize?
    @Binding var visibleVideoRect: CGRect

    func makeUIView(context: Context) -> IOSMetalVideoSurface {
        let view = IOSMetalVideoSurface()
        view.onVisibleVideoRectChanged = { rect in
            visibleVideoRect = rect
        }
        view.update(pixelBuffer: pixelBuffer, videoSize: videoSize)
        return view
    }

    func updateUIView(_ view: IOSMetalVideoSurface, context: Context) {
        view.onVisibleVideoRectChanged = { rect in
            visibleVideoRect = rect
        }
        view.update(pixelBuffer: pixelBuffer, videoSize: videoSize)
    }
}

@MainActor
final class IOSMetalVideoSurface: MTKView {
    var onVisibleVideoRectChanged: ((CGRect) -> Void)?

    private let commandQueue: MTLCommandQueue?
    private let ciContext: CIContext?
    private var pixelBuffer: CVPixelBuffer?
    private var videoSize: CGSize?
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

    func update(pixelBuffer: CVPixelBuffer?, videoSize: CGSize?) {
        self.pixelBuffer = pixelBuffer
        self.videoSize = videoSize
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

        let drawableRect = CGRect(origin: .zero, size: drawableSize)
        ciContext.draw(CIImage(color: .black), in: drawableRect, from: drawableRect)
        if let pixelBuffer {
            let image = CIImage(cvPixelBuffer: pixelBuffer)
            let targetRect = VideoGeometry.aspectFit(source: image.extent.size, in: drawableRect)
            ciContext.draw(image, in: targetRect, from: image.extent)
        }
        commandBuffer.present(drawable)
        commandBuffer.commit()
    }

    private func publishVisibleVideoRect() {
        let source = videoSize ?? pixelBuffer.map {
            CGSize(width: CVPixelBufferGetWidth($0), height: CVPixelBufferGetHeight($0))
        } ?? .zero
        let rect = VideoGeometry.aspectFit(source: source, in: bounds)
        guard rect != lastPublishedRect else { return }
        lastPublishedRect = rect
        onVisibleVideoRectChanged?(rect)
    }
}
