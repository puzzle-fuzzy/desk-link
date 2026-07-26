import AppKit
import DeskLinkAppleCore
import SwiftUI

func deskLinkApprovalForWindowPresentation(
    _ approval: HostApproval?,
    controllerState _: ConnectionState
) -> HostApproval? {
    approval
}

@MainActor
private final class DeskLinkLifecycleDelegate: NSObject, NSApplicationDelegate {
    weak var controller: ControllerBridge?
    weak var host: HostBridge?
    private var terminationStarted = false

    func configure(controller: ControllerBridge, host: HostBridge) {
        self.controller = controller
        self.host = host
    }

    func applicationWillTerminate(_ notification: Notification) {
        controller?.releaseAll()
        controller?.disconnect()
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard !terminationStarted else { return .terminateNow }
        terminationStarted = true
        Task { @MainActor [weak self] in
            self?.controller?.releaseAll()
            self?.controller?.disconnect()
            await self?.host?.shutdownAndWait()
            sender.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }
}

@main
struct DeskLinkApp: App {
    @NSApplicationDelegateAdaptor(DeskLinkLifecycleDelegate.self) private var lifecycle
    @StateObject private var controller = ControllerBridge(configuration: .macOSDefaults)
    @StateObject private var host = HostBridge()

    var body: some Scene {
        WindowGroup {
            Group {
                if isControllerSessionState(controller.state) {
                    SessionView(bridge: controller)
                } else {
                    DeskLinkShell(host: host, controller: controller)
                }
            }
            .sheet(item: pendingApproval) { approval in
                ApprovalView(bridge: host, approval: approval)
                    .padding(24)
                    .frame(minWidth: 520)
                    .interactiveDismissDisabled()
            }
            .onAppear { lifecycle.configure(controller: controller, host: host) }
            .onDisappear {
                host.shutdown()
                controller.releaseAll()
                controller.disconnect()
            }
        }
        .windowStyle(.titleBar)
        .windowToolbarStyle(.unified)
    }

    private var pendingApproval: Binding<HostApproval?> {
        Binding(
            get: {
                deskLinkApprovalForWindowPresentation(
                    host.pendingApproval,
                    controllerState: controller.state
                )
            },
            set: { _ in }
        )
    }

    private func isControllerSessionState(_ state: ConnectionState) -> Bool {
        switch state {
        case .connected, .reconnecting, .recovering, .frozen: true
        default: false
        }
    }
}
