import SwiftUI

struct ApprovalView: View {
    @ObservedObject var bridge: HostBridge
    let approval: HostApproval

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Allow this controller?").font(.headline)
            Text("Verify this fingerprint with the person requesting access before approving.")
                .foregroundStyle(.secondary)
            Text(approval.fingerprint)
                .font(.body.monospaced())
                .textSelection(.enabled)
            HStack {
                Button("Reject") { bridge.reject() }
                    .keyboardShortcut(.cancelAction)
                Button("Approve") { bridge.approve() }
                    .buttonStyle(.borderedProminent)
                    .disabled(!bridge.canApprove)
            }
            if !bridge.canApprove {
                Text("Screen Recording and Accessibility are required before approval.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding()
        .background(.quaternary, in: RoundedRectangle(cornerRadius: 12))
    }
}
