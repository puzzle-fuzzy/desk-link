import AppKit
import SwiftUI

struct ConnectView: View {
    @ObservedObject var bridge: ControllerBridge

    var body: some View {
        HStack {
            Button("Paste invitation") { connectFromPasteboard() }
                .buttonStyle(.borderedProminent)
            Text("The invitation is read from the clipboard and is never shown here.")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private func connectFromPasteboard() {
        guard let encoded = NSPasteboard.general.string(forType: .string),
              let invite = Data(base64Encoded: encoded.trimmingCharacters(in: .whitespacesAndNewlines))
        else {
            bridge.connect(invite: Data())
            return
        }
        bridge.connect(invite: invite)
    }
}
