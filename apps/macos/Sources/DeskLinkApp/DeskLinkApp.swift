import AppKit
import SwiftUI

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
    @StateObject private var controller = ControllerBridge()
    @StateObject private var host = HostBridge()
    @State private var role: AppRole?

    var body: some Scene {
        WindowGroup {
            Group {
                if let role {
                    switch role {
                    case .controller:
                        if isControllerSessionState(controller.state) {
                            SessionView(bridge: controller)
                        } else {
                            HomeView(bridge: controller, chooseRole: { self.role = nil })
                        }
                    case .host:
                        HostHomeView(bridge: host, chooseRole: { self.role = nil })
                    }
                } else {
                    RolePickerView { selectedRole in
                        role = selectedRole
                    }
                }
            }
            .onAppear { lifecycle.configure(controller: controller, host: host) }
            .onChange(of: role) { selectedRole in
                if selectedRole != .host { host.stop() }
                if selectedRole != .controller { controller.disconnect() }
            }
            .onDisappear {
                host.shutdown()
                controller.releaseAll()
                controller.disconnect()
            }
        }
    }

    private func isControllerSessionState(_ state: ConnectionState) -> Bool {
        switch state {
        case .connected, .reconnecting, .recovering, .frozen: true
        default: false
        }
    }
}
