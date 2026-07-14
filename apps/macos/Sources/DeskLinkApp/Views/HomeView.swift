import SwiftUI

struct HomeView: View {
    @ObservedObject var bridge: RustBridge

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("DeskLink")
                .font(.largeTitle.bold())
            Text(statusText)
                .foregroundStyle(.secondary)
            if let pairing = bridge.pairing {
                Text("Pairing code: \(pairing.code)")
                    .font(.title2.monospaced())
            }
            HStack {
                Button("Start pairing") { bridge.startPairing() }
                Button("Connect") { bridge.connect(code: "") }
            }
            if let error = bridge.lastError {
                Text(error).foregroundStyle(.red)
            }
        }
        .padding(28)
        .frame(minWidth: 420, minHeight: 240)
    }

    private var statusText: String {
        switch bridge.state {
        case .idle: "Ready"
        case .pairing: "Waiting for a controller"
        case .connecting: "Connecting"
        case let .connected(streamID): "Connected · stream \(streamID)"
        case .closed: "Disconnected"
        case let .failed(message): message
        }
    }
}
