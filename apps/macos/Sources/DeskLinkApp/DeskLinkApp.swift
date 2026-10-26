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
        controller?.disconnect()
    }

    func applicationDidResignActive(_ notification: Notification) {
        controller?.cancelPendingPaste()
        controller?.releaseAll()
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard !terminationStarted else { return .terminateNow }
        terminationStarted = true
        Task { @MainActor [weak self] in
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
    @StateObject private var controller: ControllerBridge
    @StateObject private var host: HostBridge
    @StateObject private var account: AccountClient

    init() {
#if DEBUG
        if MacHostAcceptance.isEnabled {
            _controller = StateObject(wrappedValue: ControllerBridge.testing(state: .idle))
            _host = StateObject(wrappedValue: MacHostAcceptance.host())
            let account = AccountClient(baseURL: URL(string: "https://example.invalid")!, platform: .macos,
                deviceName: "Host acceptance", keychain: InMemoryKeychainStore())
            account.skipLogin()
            _account = StateObject(wrappedValue: account)
            return
        }
        if MacSessionAcceptance.isEnabled {
            _controller = StateObject(wrappedValue: MacSessionAcceptance.controller())
            _host = StateObject(wrappedValue: HostBridge.testing(permissionSnapshot: .denied))
            let account = AccountClient(
                baseURL: URL(string: "https://example.invalid")!,
                platform: .macos,
                deviceName: "Session acceptance",
                keychain: InMemoryKeychainStore()
            )
            account.skipLogin()
            _account = StateObject(wrappedValue: account)
            return
        }
#endif
        _controller = StateObject(wrappedValue: ControllerBridge(configuration: .macOSDefaults))
        _host = StateObject(wrappedValue: HostBridge())
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
                    AccountLoginView(account: account, allowsSkipLogin: true)
                case .signedIn, .skipped, .offline:
                    VStack(spacing: 0) {
                        AccountRecoveryNotice(account: account)
                        workspaceContent
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
                controller.disconnect()
            }
            .task {
#if DEBUG
                if MacSessionAcceptance.isEnabled || MacHostAcceptance.isEnabled { return }
#endif
                await account.restore()
            }
        }
        .defaultSize(width: 1080, height: 700)
        .windowStyle(.titleBar)
        .windowToolbarStyle(.unified)
    }

    @ViewBuilder
    private var workspaceContent: some View {
#if DEBUG
        if MacHostAcceptance.isEnabled {
            HostHomeView(bridge: host, page: .connection)
                .frame(minWidth: 980, minHeight: 620)
        } else if MacSessionAcceptance.isEnabled {
            // Keep the acceptance process isolated even after disconnecting or
            // beginning termination; the regular workspace reads saved hosts.
            SessionView(bridge: controller, host: host, account: account,
                        clipboard: MacSessionAcceptance.clipboard())
        } else {
            regularWorkspaceContent
        }
#else
        regularWorkspaceContent
#endif
    }

    @ViewBuilder
    private var regularWorkspaceContent: some View {
        if isControllerSessionState(controller.state) {
            SessionView(bridge: controller, host: host, account: account)
        } else {
            DeskLinkShell(host: host, controller: controller, account: account)
        }
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
