import CoreImage
import DeskLinkAppleCore
import CoreVideo
@preconcurrency import MetalKit
import SwiftUI

struct MetalVideoView: NSViewRepresentable {
    let frame: PresentedVideoFrame?
    let viewport: SessionViewport
    let onFramePresented: @MainActor @Sendable (VideoFramePresentationToken, CGRect) -> Bool

    func makeNSView(context: Context) -> MTKView {
        let view = MTKView(frame: .zero, device: MTLCreateSystemDefaultDevice())
        view.enableSetNeedsDisplay = true
        view.isPaused = true
        view.framebufferOnly = false
        view.autoResizeDrawable = true
        view.colorPixelFormat = .bgra8Unorm
        view.delegate = context.coordinator
        return view
    }

    func updateNSView(_ view: MTKView, context: Context) {
        context.coordinator.onFramePresented = onFramePresented
        guard context.coordinator.pixelBuffer !== frame?.pixelBuffer
                || context.coordinator.presentationToken != frame?.token
                || context.coordinator.viewport != viewport else { return }
        context.coordinator.frame = frame
        context.coordinator.viewport = viewport
        view.setNeedsDisplay(view.bounds)
    }

    func makeCoordinator() -> Renderer {
        Renderer()
    }

    final class Renderer: NSObject, MTKViewDelegate, @unchecked Sendable {
        var frame: PresentedVideoFrame?
        var pixelBuffer: CVPixelBuffer? { frame?.pixelBuffer }
        var presentationToken: VideoFramePresentationToken? { frame?.token }
        var onFramePresented: (@MainActor @Sendable (VideoFramePresentationToken, CGRect) -> Bool)?
        var viewport = SessionViewport(sourceSize: .zero, viewportSize: .zero)
        private var lastPresentedTime: CFTimeInterval = 0
        private let context = CIContext()
        private var commandQueue: MTLCommandQueue?

        func draw(in view: MTKView) {
            guard let drawable = view.currentDrawable else { return }
            if commandQueue == nil {
                commandQueue = drawable.texture.device.makeCommandQueue()
            }
            guard let commandBuffer = commandQueue?.makeCommandBuffer() else { return }
            let frame = self.frame
            let token = frame?.token
            guard let pixelBuffer = frame?.pixelBuffer else {
                let target = CGRect(origin: .zero, size: view.drawableSize)
                context.render(
                    CIImage(color: .black).cropped(to: target),
                    to: drawable.texture,
                    commandBuffer: commandBuffer,
                    bounds: target,
                    colorSpace: CGColorSpaceCreateDeviceRGB()
                )
                commandBuffer.present(drawable)
                commandBuffer.commit()
                return
            }
            let image = CIImage(cvPixelBuffer: pixelBuffer)
            let target = CGRect(origin: .zero, size: view.drawableSize)
            let fitted = viewport.drawableVideoRect(drawableSize: view.drawableSize)
            guard !fitted.isEmpty else { return }
            let normalized = image.transformed(
                by: CGAffineTransform(
                    translationX: -image.extent.minX,
                    y: -image.extent.minY
                )
            )
            let scaled = normalized.transformed(
                by: CGAffineTransform(
                    scaleX: fitted.width / image.extent.width,
                    y: fitted.height / image.extent.height
                )
            )
            let positioned = scaled.transformed(
                by: CGAffineTransform(translationX: fitted.minX, y: fitted.minY)
            )
            let output = positioned.composited(
                over: CIImage(color: .black).cropped(to: target)
            )
            context.render(
                output,
                to: drawable.texture,
                commandBuffer: commandBuffer,
                bounds: target,
                colorSpace: CGColorSpaceCreateDeviceRGB()
            )
            if let token, let receipt = onFramePresented {
                let scaleX = target.width > 0 ? view.bounds.width / target.width : 0
                let scaleY = target.height > 0 ? view.bounds.height / target.height : 0
                let presentedRect = CGRect(x: fitted.minX * scaleX, y: fitted.minY * scaleY,
                                           width: fitted.width * scaleX, height: fitted.height * scaleY)
                drawable.addPresentedHandler { [weak self] drawable in
                    let presentedTime = drawable.presentedTime
                    guard presentedTime > 0, !presentedRect.isEmpty else { return } // Dropped drawables never activate input.
                    Task { @MainActor [weak self] in
                        guard let self, presentedTime >= self.lastPresentedTime,
                              self.presentationToken?.context == token.context,
                              receipt(token, presentedRect) else { return }
                        self.lastPresentedTime = presentedTime
                    }
                }
            }
            commandBuffer.present(drawable)
            commandBuffer.commit()
        }

        func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {
            view.setNeedsDisplay(view.bounds)
        }
    }
}
