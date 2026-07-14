import SwiftUI

struct SessionView: View {
    @ObservedObject var bridge: RustBridge

    var body: some View {
        VStack(spacing: 0) {
            MetalVideoView(pixelBuffer: bridge.latestPixelBuffer)
                .background(.black)
                .overlay(alignment: .topTrailing) {
                    Text("DeskLink")
                        .padding(8)
                        .background(.black.opacity(0.55), in: Capsule())
                        .padding()
                }
            HStack {
                Text("Frames: \(bridge.metrics.receivedFrames)")
                Spacer()
                Button("Keyframe") { bridge.requestKeyframe() }
                Button("Disconnect") { bridge.disconnect() }
            }
            .padding()
        }
        .onDisappear { bridge.releaseAll() }
    }
}
