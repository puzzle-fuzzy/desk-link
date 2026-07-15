import CoreImage
import CoreVideo
import MetalKit
import SwiftUI

struct MetalVideoView: NSViewRepresentable {
    let pixelBuffer: CVPixelBuffer?

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
        context.coordinator.pixelBuffer = pixelBuffer
        view.setNeedsDisplay(view.bounds)
    }

    func makeCoordinator() -> Renderer {
        Renderer()
    }

    final class Renderer: NSObject, MTKViewDelegate {
        var pixelBuffer: CVPixelBuffer?
        private let context = CIContext()
        private var commandQueue: MTLCommandQueue?

        func draw(in view: MTKView) {
            guard let drawable = view.currentDrawable,
                  let pixelBuffer
            else { return }
            if commandQueue == nil {
                commandQueue = drawable.texture.device.makeCommandQueue()
            }
            guard let commandBuffer = commandQueue?.makeCommandBuffer() else { return }
            let image = CIImage(cvPixelBuffer: pixelBuffer)
            let target = CGRect(origin: .zero, size: view.drawableSize)
            let fitted = VideoGeometry.aspectFit(source: image.extent.size, in: target)
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
            commandBuffer.present(drawable)
            commandBuffer.commit()
        }

        func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {}
    }
}
