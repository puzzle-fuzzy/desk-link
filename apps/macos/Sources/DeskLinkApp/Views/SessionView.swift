import CoreVideo
import SwiftUI

struct SessionView: View {
    @ObservedObject var bridge: RustBridge
    @State private var leftButtonPressed = false

    var body: some View {
        VStack(spacing: 0) {
            GeometryReader { geometry in
                MetalVideoView(pixelBuffer: bridge.latestPixelBuffer)
                    .background(.black)
                    .contentShape(Rectangle())
                    .onContinuousHover { phase in
                        if case let .active(location) = phase {
                            sendPointer(at: location, in: geometry.size)
                        }
                    }
                    .gesture(
                        DragGesture(minimumDistance: 0)
                            .onChanged { value in
                                guard sendPointer(at: value.location, in: geometry.size) else {
                                    return
                                }
                                if !leftButtonPressed {
                                    leftButtonPressed = true
                                    bridge.sendMouseButton(1, pressed: true)
                                }
                            }
                            .onEnded { value in
                                _ = sendPointer(at: value.location, in: geometry.size)
                                if leftButtonPressed {
                                    bridge.sendMouseButton(1, pressed: false)
                                    leftButtonPressed = false
                                }
                            }
                    )
                    .overlay(alignment: .topTrailing) {
                        Text("DeskLink")
                            .padding(8)
                            .background(.black.opacity(0.55), in: Capsule())
                            .padding()
                    }
            }
            HStack {
                Text("Frames: \(bridge.metrics.receivedFrames)")
                Spacer()
                Button("Keyframe") { bridge.requestKeyframe() }
                Button("Disconnect") { bridge.disconnect() }
            }
            .padding()
        }
        .frame(minWidth: 720, minHeight: 480)
        .onDisappear {
            if leftButtonPressed {
                bridge.sendMouseButton(1, pressed: false)
                leftButtonPressed = false
            }
            bridge.releaseAll()
        }
    }

    @discardableResult
    private func sendPointer(at location: CGPoint, in size: CGSize) -> Bool {
        guard let pixelBuffer = bridge.latestPixelBuffer else { return false }
        let source = CGSize(
            width: CGFloat(CVPixelBufferGetWidth(pixelBuffer)),
            height: CGFloat(CVPixelBufferGetHeight(pixelBuffer))
        )
        let videoRect = VideoGeometry.aspectFit(
            source: source,
            in: CGRect(origin: .zero, size: size)
        )
        guard let normalized = InputMapper(videoRect: videoRect).normalizedPoint(for: location) else {
            return false
        }
        bridge.sendMouseMove(x: Float(normalized.x), y: Float(normalized.y))
        return true
    }
}
