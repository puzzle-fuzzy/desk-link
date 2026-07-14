import SwiftUI

struct DiagnosticsView: View {
    @ObservedObject var bridge: RustBridge

    var body: some View {
        Form {
            LabeledContent("State", value: stateText)
            LabeledContent("Received frames", value: "\(bridge.metrics.receivedFrames)")
            LabeledContent("Dropped frames", value: "\(bridge.metrics.droppedFrames)")
            Button("Request keyframe") { bridge.requestKeyframe() }
        }
        .padding()
    }

    private var stateText: String {
        String(describing: bridge.state)
    }
}
