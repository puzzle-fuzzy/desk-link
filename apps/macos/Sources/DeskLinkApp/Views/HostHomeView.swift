import AppKit
import SwiftUI

struct HostHomeView: View {
    @ObservedObject var bridge: HostBridge
    let chooseRole: () -> Void

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                HStack {
                    Text("Share this Mac").font(.largeTitle.bold())
                    Spacer()
                    Button("Change role", action: chooseRole)
                }
                permissionCard(
                    title: "Screen Recording",
                    granted: bridge.permissions.screenRecording == .granted,
                    request: bridge.requestScreenRecording,
                    settingsURL: bridge.permissions.screenRecordingSettingsURL
                )
                permissionCard(
                    title: "Accessibility",
                    granted: bridge.permissions.accessibility == .granted,
                    request: bridge.requestAccessibility,
                    settingsURL: bridge.permissions.accessibilitySettingsURL
                )
                HStack {
                    Button("Create invitation") { bridge.createInvite() }
                        .buttonStyle(.borderedProminent)
                        .disabled(bridge.pairingInvite != nil)
                    if let invite = bridge.pairingInvite {
                        Button("Copy invitation") { bridge.copyInviteToPasteboard() }
                        Text("Expires \(invite.expiresAt.formatted(date: .omitted, time: .shortened))")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Button("Cancel") { bridge.stop() }
                    }
                    Spacer()
                    Button("Stop sharing") { bridge.stop() }
                        .disabled(bridge.state == .idle || bridge.state == .closed)
                }
                Text(statusText).foregroundStyle(.secondary)
                if let approval = bridge.pendingApproval {
                    ApprovalView(bridge: bridge, approval: approval)
                }
                if !bridge.trustedControllers.isEmpty {
                    Text("Trusted controllers").font(.headline)
                    ForEach(bridge.trustedControllers, id: \.deviceID) { controller in
                        HStack {
                            Text(controller.displayName)
                            Spacer()
                            Button("Revoke") { bridge.revoke(controller: controller) }
                        }
                    }
                }
                VStack(alignment: .leading, spacing: 4) {
                    Text("Host diagnostics").font(.headline)
                    Text("Video packets: \(bridge.metrics.sentVideoPackets) · Input events: \(bridge.metrics.receivedInputEvents)")
                        .font(.caption)
                    Text("Keyframe requests: \(bridge.metrics.keyframeRequests)")
                        .font(.caption)
                }
                if let error = bridge.lastError { Text(error).foregroundStyle(.red) }
            }
            .padding(28)
        }
        .frame(minWidth: 600, minHeight: 500)
        .onAppear { bridge.refreshPermissions() }
    }

    private func permissionCard(title: String, granted: Bool, request: @escaping () -> Void, settingsURL: URL) -> some View {
        HStack {
            VStack(alignment: .leading) {
                Text(title).font(.headline)
                Text(granted ? "Granted" : "Required before remote access can begin.")
                    .font(.caption)
                    .foregroundStyle(granted ? .green : .secondary)
            }
            Spacer()
            if !granted {
                Button("Allow", action: request)
                Button("Open Settings") { NSWorkspace.shared.open(settingsURL) }
            }
        }
        .padding()
        .background(.quaternary, in: RoundedRectangle(cornerRadius: 12))
    }

    private var statusText: String {
        switch bridge.state {
        case .idle: "Ready to create an invitation"
        case .connecting: "Connecting to the relay"
        case .waitingForApproval: "Waiting for your decision"
        case .negotiating: "Preparing the remote session"
        case .connected: "Sharing is active"
        case .stopping: "Stopping"
        case .closed: "Stopped"
        case let .failed(message): message
        }
    }
}
