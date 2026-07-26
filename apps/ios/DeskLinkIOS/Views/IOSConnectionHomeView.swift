import DeskLinkAppleCore
import UIKit
import SwiftUI

enum IOSRootDestination: String, CaseIterable, Hashable {
    case connect = "连接设备"
    case savedHosts = "已保存设备"
    case more = "更多"
}

struct IOSRootView: View {
    @ObservedObject var controller: ControllerBridge
    @State private var destination: IOSRootDestination = .connect

    var body: some View {
        TabView(selection: $destination) {
            IOSConnectionHomeView(controller: controller)
                .tabItem { Label(IOSRootDestination.connect.rawValue, systemImage: "rectangle.portrait.and.arrow.right") }
                .tag(IOSRootDestination.connect)
            IOSSavedHostsView(controller: controller, destination: $destination)
                .tabItem { Label(IOSRootDestination.savedHosts.rawValue, systemImage: "externaldrive.connected.to.line.below") }
                .tag(IOSRootDestination.savedHosts)
            IOSMoreView(controller: controller)
                .tabItem { Label(IOSRootDestination.more.rawValue, systemImage: "ellipsis.circle") }
                .tag(IOSRootDestination.more)
        }
    }
}

struct IOSConnectionHomeView: View {
    @ObservedObject var controller: ControllerBridge
    @State private var inviteText = ""
    @State private var savedHosts: [SavedHost] = []
    @State private var isShowingScanner = false
    @State private var inputError: String?
    @FocusState private var isInviteFieldFocused: Bool

    private let savedHostStore = SavedHostStore()

    var body: some View {
        if IOSSessionPresentation.isActive(controller.state) {
            IOSSessionView(controller: controller)
        } else {
            NavigationStack {
            Form {
                Section("连接设备") {
                    TextField("粘贴或输入连接码", text: $inviteText, axis: .vertical)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .focused($isInviteFieldFocused)
                        .font(.footnote.monospaced())
                    HStack(spacing: 18) {
                        Button("从剪贴板粘贴") { pasteInvite() }
                            .buttonStyle(.borderless)
                            .accessibilityIdentifier("paste-invite")
                        Button("扫描二维码") {
                            beginScanning()
                        }
                        .buttonStyle(.borderless)
                        .accessibilityIdentifier("scan-invite")
                    }
                    Button("开始连接") { connectInviteText() }
                        .disabled(inviteText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    if isBusy {
                        HStack(spacing: 8) {
                            ProgressView()
                            Text(statusText)
                                .foregroundStyle(.secondary)
                        }
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel(statusText)
                    }
                    Button("断开连接", role: .destructive) { controller.disconnect() }
                        .disabled(!isConnectedOrConnecting)
                }

                Section("状态") {
                    Label(statusText, systemImage: statusSymbol)
                    if let error = inputError ?? controller.lastError {
                        Text(error).foregroundStyle(.red)
                    }
                }

                if !savedHosts.isEmpty {
                    Section("最近设备") {
                        ForEach(savedHosts.prefix(3)) { host in
                            Button {
                                controller.connect(savedHost: host)
                            } label: {
                                VStack(alignment: .leading, spacing: 3) {
                                    Text(host.serverName)
                                        .foregroundStyle(.primary)
                                    Text("已保存的安全连接")
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                            }
                        }
                    }
                }
            }
            .navigationTitle("DeskLink")
            .sheet(isPresented: $isShowingScanner) {
                NavigationStack {
                    IOSQRCodeScanner { invite in
                        inviteText = invite.base64EncodedString()
                        isShowingScanner = false
                        connect(invite: invite)
                    } onInvalidPayload: {
                        inputError = "二维码不是有效的 DeskLink 连接码。"
                    }
                    .navigationTitle("扫描二维码")
                    .toolbar {
                        ToolbarItem(placement: .cancellationAction) {
                            Button("取消") { isShowingScanner = false }
                        }
                    }
                }
            }
            .task { reloadSavedHosts() }
            .onChange(of: controller.state) { _ in reloadSavedHosts() }
            }
        }
    }

    private var statusText: String {
        IOSSessionPresentation.statusText(controller.state)
    }

    private var statusSymbol: String {
        switch controller.state {
        case .connected: "checkmark.circle.fill"
        case .failed: "exclamationmark.triangle.fill"
        case .pairing: "person.crop.circle.badge.questionmark"
        case .connecting, .reconnecting, .recovering: "arrow.triangle.2.circlepath"
        default: "circle"
        }
    }

    private var isBusy: Bool {
        switch controller.state {
        case .pairing, .connecting, .reconnecting, .recovering: true
        default: false
        }
    }

    private var isConnectedOrConnecting: Bool {
        switch controller.state {
        case .pairing, .connecting, .connected, .reconnecting, .recovering, .frozen: true
        default: false
        }
    }

    private func pasteInvite() {
        inputError = nil
        inviteText = UIPasteboard.general.string ?? ""
        if inviteText.isEmpty { inputError = "剪贴板中没有连接码。" }
    }

    private func beginScanning() {
        isInviteFieldFocused = false
        UIApplication.shared.sendAction(
            #selector(UIResponder.resignFirstResponder),
            to: nil,
            from: nil,
            for: nil
        )
        isShowingScanner = true
    }

    private func connectInviteText() {
        connect(invite: IOSPairingInput.decodeInvite(inviteText) ?? Data())
    }

    private func connect(invite: Data) {
        inputError = nil
        controller.connect(invite: invite)
    }

    private func reloadSavedHosts() {
        savedHosts = (try? savedHostStore.loadAll()) ?? []
    }
}

struct IOSMoreView: View {
    @ObservedObject var controller: ControllerBridge

    var body: some View {
        NavigationStack {
            Form {
                Section("DeskLink") {
                    Label("iPhone 仅作为远程控制端", systemImage: "iphone")
                    Text("连接码只用于一次配对；认证后的设备材料保存在系统 Keychain 中。")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
                Section("当前连接") {
                    Text(connectionSummary)
                    Button("断开连接", role: .destructive) { controller.disconnect() }
                        .disabled(!isActive)
                }
            }
            .navigationTitle("更多")
        }
    }

    private var isActive: Bool {
        switch controller.state {
        case .pairing, .connecting, .connected, .reconnecting, .recovering, .frozen: true
        default: false
        }
    }

    private var connectionSummary: String {
        switch controller.state {
        case .pairing, .connecting, .reconnecting, .recovering, .frozen:
            IOSSessionPresentation.statusText(controller.state)
        case .connected: "已连接"
        case .failed: "连接失败"
        case .closed: "已断开"
        default: "未连接"
        }
    }
}
