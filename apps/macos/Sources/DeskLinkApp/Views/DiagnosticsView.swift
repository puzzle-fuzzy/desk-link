import SwiftUI

struct DiagnosticsView: View {
    @ObservedObject var bridge: ControllerBridge

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Diagnostics").font(.headline)
            LabeledContent("State", value: stateText)
            LabeledContent("Received frames", value: "\(bridge.metrics.receivedFrames)")
            LabeledContent("Dropped frames", value: "\(bridge.metrics.droppedFrames)")
            LabeledContent("Stream ID", value: streamID.map(String.init) ?? "—")
            LabeledContent("Config ID", value: "Not announced")
            LabeledContent("Error category", value: errorCategory)
            Button("Request keyframe") { bridge.requestKeyframe() }
        }
        .font(.caption)
        .padding(.top, 4)
    }

    private var streamID: UInt64? {
        if case let .connected(streamID) = bridge.state { return streamID }
        return nil
    }

    private var stateText: String {
        switch bridge.state {
        case .idle: "Idle"
        case .pairing: "Awaiting approval"
        case .connecting: "Connecting"
        case .connected: "Connected"
        case .reconnecting: "Reconnecting"
        case .recovering: "Recovering video"
        case .frozen: "Frozen"
        case .closed: "Disconnected"
        case .failed: "Failed"
        }
    }

    private var errorCategory: String {
        let error = bridge.userFacingError
        guard !error.isEmpty else { return "None" }
        if error.localizedCaseInsensitiveContains("secure") { return "Security" }
        if error.localizedCaseInsensitiveContains("video") { return "Video" }
        return "Connection"
    }
}
