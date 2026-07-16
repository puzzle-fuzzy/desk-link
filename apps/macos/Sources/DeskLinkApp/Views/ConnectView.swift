import AppKit
import SwiftUI

struct ConnectView: View {
    @ObservedObject var bridge: ControllerBridge

    var body: some View {
        Button("粘贴连接码") { connectFromPasteboard() }
            .buttonStyle(DeskLinkPrimaryButtonStyle())
            .help("从剪贴板读取完整连接码并开始连接")
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
