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
            if let verifyKey = bridge.controllerVerifyKeyHex {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Controller verify key")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Text(verifyKey)
                        .font(.caption.monospaced())
                        .textSelection(.enabled)
                }
            }
            HStack {
                Button("Start pairing") { bridge.startPairing() }
                Button("Connect securely") { bridge.connectSecureFromEnvironment() }
            }
            Text("Secure connection uses the DESKLINK_* environment configuration.")
                .font(.caption)
                .foregroundStyle(.secondary)
            if let error = bridge.lastError {
                Text(error).foregroundStyle(.red)
            }
        }
        .padding(28)
        .frame(minWidth: 540, minHeight: 280)
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
