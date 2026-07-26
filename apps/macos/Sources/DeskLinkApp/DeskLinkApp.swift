import AppKit
import DeskLinkAppleCore
import SwiftUI

private enum MacAccountConfiguration {
    static var accountURL: URL {
        let value = (Bundle.main.object(forInfoDictionaryKey: "DeskLinkAccountURL") as? String)
            ?? "https://account.p2p.yxswy.com"
        return URL(string: value) ?? URL(string: "https://account.p2p.yxswy.com")!
    }
}

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
    @StateObject private var account: AccountClient

    init() {
        _account = StateObject(wrappedValue: AccountClient(
            baseURL: MacAccountConfiguration.accountURL,
            platform: .macos,
            deviceName: Host.current().localizedName ?? "Mac"
        ))
    }

    var body: some Scene {
        WindowGroup {
            Group {
                switch account.state {
                case .loading:
                    ProgressView("正在准备 DeskLink…")
                case .signedOut:
                    AccountLoginView(account: account)
                case .signedIn:
                    if isControllerSessionState(controller.state) {
                        SessionView(bridge: controller, host: host, account: account)
                    } else {
                        DeskLinkShell(host: host, controller: controller, account: account)
                    }
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
            .task { await account.restore() }
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
