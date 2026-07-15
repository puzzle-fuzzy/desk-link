import CoreVideo
import SwiftUI

struct SessionView: View {
    @ObservedObject var bridge: ControllerBridge

    var body: some View {
        VStack(spacing: 0) {
            ZStack {
                MetalVideoView(pixelBuffer: bridge.latestPixelBuffer)
                    .background(.black)
                SessionInputView(bridge: bridge, videoSize: videoSize)
                    .background(Color.clear)
                VStack {
                    HStack {
                        Spacer()
                        Text(statusText)
                            .padding(8)
                            .background(.black.opacity(0.55), in: Capsule())
                    }
                    Spacer()
                }
                .padding()
                .allowsHitTesting(false)
            }
            HStack {
                Text("Frames: \(bridge.metrics.receivedFrames) · Dropped: \(bridge.metrics.droppedFrames)")
                Spacer()
                Button("Keyframe") { bridge.requestKeyframe() }
                Button("Disconnect") { bridge.disconnect() }
            }
            .padding()
        }
        .frame(minWidth: 720, minHeight: 480)
        .onDisappear { bridge.releaseAll() }
    }

    private var videoSize: CGSize? {
        guard let pixelBuffer = bridge.latestPixelBuffer else { return nil }
        return CGSize(width: CVPixelBufferGetWidth(pixelBuffer), height: CVPixelBufferGetHeight(pixelBuffer))
    }

    private var statusText: String {
        switch bridge.state {
        case let .connected(streamID): "Connected · stream \(streamID)"
        case .reconnecting: "Reconnecting"
        case .recovering: "Recovering video"
        case .frozen: "Video frozen"
        default: "DeskLink"
        }
    }
}
