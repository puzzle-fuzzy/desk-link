import AppKit
import SwiftUI

struct ControllerHomeView: View {
    @ObservedObject var bridge: ControllerBridge
    let chooseRole: () -> Void
    @State private var savedHosts: [SavedHost] = []
    @State private var storeError: String?
    private let savedHostStore = SavedHostStore()

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack {
                Text("Control a Mac").font(.largeTitle.bold())
                Spacer()
                Button("Change role", action: chooseRole)
            }
            ConnectView(bridge: bridge)
            if let verifyKey = bridge.controllerVerifyKeyHex {
                LabeledContent("This Mac's verification key") {
                    Text(verifyKey).font(.caption.monospaced()).textSelection(.enabled)
                }
            }
            if savedHosts.isEmpty {
                Text("No approved Macs are saved on this device.")
                    .foregroundStyle(.secondary)
            } else {
                Text("Approved Macs").font(.headline)
                ForEach(savedHosts) { host in
                    HStack {
                        Text(host.serverName)
                        Spacer()
                        Button(bridge.state == .closed ? "Reconnect" : "Connect") {
                            bridge.connect(savedHost: host)
                        }
                    }
                }
            }
            if let error = bridge.lastError ?? storeError {
                Text(error).foregroundStyle(.red)
            }
            DiagnosticsView(bridge: bridge)
        }
        .padding(28)
        .frame(minWidth: 560, minHeight: 360)
        .onAppear(perform: loadSavedHosts)
    }

    private func loadSavedHosts() {
        do {
            savedHosts = try savedHostStore.loadAll()
            storeError = nil
        } catch {
            savedHosts = []
            storeError = "Approved Macs could not be loaded."
        }
    }
}
