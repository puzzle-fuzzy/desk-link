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

        func draw(in view: MTKView) {
            guard let drawable = view.currentDrawable,
                  let commandBuffer = view.currentDrawable?.texture.device.makeCommandQueue()?.makeCommandBuffer(),
                  let pixelBuffer
            else { return }
            let image = CIImage(cvPixelBuffer: pixelBuffer)
            context.render(
                image,
                to: drawable.texture,
                commandBuffer: commandBuffer,
                bounds: image.extent,
                colorSpace: CGColorSpaceCreateDeviceRGB()
            )
            commandBuffer.present(drawable)
            commandBuffer.commit()
        }

        func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {}
    }
}
