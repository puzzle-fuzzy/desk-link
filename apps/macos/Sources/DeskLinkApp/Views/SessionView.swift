import CoreVideo
import DeskLinkAppleCore
import SwiftUI

let deskLinkSessionSafetyCopy = "退出窗口前，DeskLink 会释放所有按键与鼠标状态。"

func deskLinkSessionStatusText(for state: ConnectionState) -> String {
    deskLinkConnectionStatus(for: state).title
}

@MainActor
private final class SessionVideoPresentation: ObservableObject {
    @Published var rect: CGRect = .zero
}

@MainActor
struct SessionView: View {
    @ObservedObject var bridge: ControllerBridge
    @ObservedObject var host: HostBridge
    @ObservedObject var account: AccountClient
    @State private var modifierMode = ModifierMode.automatic
    @State private var operations = SessionOperationStatus()
    @StateObject private var videoPresentation = SessionVideoPresentation()
    private var presentedVideoRect: CGRect { videoPresentation.rect }

    var body: some View {
        VStack(spacing: 0) {
            ZStack {
                MetalVideoView(frame: bridge.videoFrame, onFramePresented: { [bridge, videoPresentation] token, rect in
                    guard bridge.confirmVideoFramePresentation(token) else { return false }
                    videoPresentation.rect = rect
                    return true
                })
                    .background(Color.black)
                SessionInputView(bridge: bridge, videoSize: videoSize, visibleVideoRect: presentedVideoRect, modifierMode: modifierMode)
                    .background(Color.clear)
                RemoteCursorPositionLayer(
                    overlay: bridge.cursorOverlay,
                    activeStreamID: cursorStreamID,
                    videoRect: presentedVideoRect
                )
                .allowsHitTesting(false)
                if bridge.latestPixelBuffer == nil {
                    VStack(spacing: 10) {
                        ProgressView()
                            .controlSize(.small)
                        Text(videoPlaceholder)
                            .font(.system(size: 13))
                            .foregroundStyle(Color.white.opacity(0.88))
                    }
                    .allowsHitTesting(false)
                }
            }

            Rectangle().fill(DeskLinkPalette.border).frame(height: 1)

            if let restriction = bridge.inputRestriction {
                Text(restriction.message)
                    .font(.system(size: 12))
                    .foregroundStyle(DeskLinkPalette.warning)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 20).padding(.vertical, 6)
                    .accessibilityIdentifier("remote-input-restriction")
            }
            if let message = operations.message ?? bridge.lastError {
                Text(message)
                    .font(.system(size: 12))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 20).padding(.vertical, 6)
                    .accessibilityIdentifier("session-operation-status")
            }

            HStack(alignment: .top) {
                DisclosureGroup("会话诊断") {
                    HStack {
                        Text("已接收 \(bridge.metrics.receivedFrames) 帧")
                        Text("已丢弃 \(bridge.metrics.droppedFrames) 帧")
                    }
                    .padding(.top, 4)
                }
                Spacer()
                Text(deskLinkSessionSafetyCopy)
            }
            .font(.system(size: 11))
            .foregroundStyle(DeskLinkPalette.mutedInk)
            .padding(.horizontal, 20)
            .frame(height: 38)
            .background(DeskLinkPalette.subtle)
        }
        .frame(minWidth: 760, minHeight: 520)
        .onChange(of: bridge.state) { _ in
            if !sessionOperationsAvailable { operations.reset() }
        }
        .onChange(of: bridge.qualityStateRevision) { _ in operations.qualityConfirmed(bridge.confirmedQuality) }
        .onChange(of: bridge.displayStateRevision) { _ in operations.displayConfirmed(bridge.activeDisplayID) }
        .task(id: operations.pendingQuality) {
            guard let requested = operations.pendingQuality else { return }
            try? await Task.sleep(for: .seconds(5))
            guard !Task.isCancelled, operations.pendingQuality == requested else { return }
            operations.qualityTimedOut()
        }
        .task(id: operations.pendingDisplayID) {
            guard let requested = operations.pendingDisplayID else { return }
            try? await Task.sleep(for: .seconds(5))
            guard !Task.isCancelled, operations.pendingDisplayID == requested else { return }
            operations.displayTimedOut()
        }
        .toolbar {
            ToolbarItem(placement: .automatic) {
                HStack(spacing: 8) {
                    DeskLinkStatusLight(color: sessionStatusColor)
                    Text(statusText)
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(DeskLinkPalette.secondaryInk)
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Menu("显示器") {
                    if bridge.remoteDisplays.isEmpty { Text("正在等待远端显示器列表") }
                    ForEach(bridge.remoteDisplays) { display in
                        Button {
                            selectDisplay(display.id)
                        } label: {
                            HStack {
                                Text(displayTitle(display))
                                if display.id == bridge.activeDisplayID { Image(systemName: "checkmark") }
                            }
                        }
                        .disabled(display.id == bridge.activeDisplayID)
                    }
                }
                .disabled(!sessionOperationsAvailable || bridge.remoteDisplays.isEmpty || operations.hasPending)
            }
            ToolbarItem(placement: .primaryAction) {
                Menu("画质") {
                    ForEach(SessionVideoQualityPreference.allCases) { preference in
                        Button {
                            selectQuality(preference)
                        } label: {
                            HStack {
                                Text(preference.title)
                                if bridge.confirmedQuality?.preference == preference { Image(systemName: "checkmark") }
                            }
                        }
                        .disabled(bridge.confirmedQuality?.preference == preference)
                    }
                    Divider()
                    if let quality = bridge.confirmedQuality {
                        Text("远端已确认：\(quality.preference.title) · 当前\(quality.preset.title)")
                    } else {
                        Text("正在等待远端画质状态")
                    }
                }
                .disabled(!sessionOperationsAvailable || operations.hasPending)
            }
            ToolbarItem(placement: .primaryAction) {
                Menu("键盘") {
                    Picker("按键映射", selection: $modifierMode) {
                        ForEach(ModifierMode.allCases) { mode in Text(mode.title).tag(mode) }
                    }
                    if bridge.remotePlatform == .windows {
                        Text("按本机习惯：Command 对应 Ctrl")
                        Divider()
                        Button("切换窗口（Alt+Tab）") { sendSystemKey(DeskLinkKeyCode.tab, modifiers: .option) }
                        Button("Windows 键") { sendSystemKey(DeskLinkKeyCode.meta) }
                        Button("运行（Win+R）") {
                            guard let context = bridge.captureInputContext() else { return }
                            bridge.send(input: .character(114, pressed: true, modifiers: .meta), context: context)
                            bridge.send(input: .character(114, pressed: false, modifiers: .meta), context: context)
                        }
                    } else if bridge.remotePlatform != .unknown {
                        Divider()
                        Button("切换应用（Command+Tab）") { sendSystemKey(DeskLinkKeyCode.tab, modifiers: .meta) }
                        Button("Command 键") { sendSystemKey(DeskLinkKeyCode.meta) }
                    }
                }
                .disabled(!keyboardAvailable)
            }
            ToolbarItem(placement: .primaryAction) {
                Button("请求关键帧") { bridge.requestKeyframe() }
                    .buttonStyle(.bordered)
            }
            ToolbarItem(placement: .primaryAction) {
                Button(deskLinkConnectionIsWaiting(bridge.state) ? "取消连接" : "断开连接") {
                    bridge.disconnect()
                }
                    .buttonStyle(.borderedProminent)
                    .accessibilityIdentifier("cancel-controller-session")
            }
            if account.state.hasAccountSession {
                ToolbarItem(placement: .primaryAction) {
                    Button("退出登录") {
                        Task { @MainActor in
                            bridge.clearRemoteLinksForLogout()
                            let stoppingHost = host
                            async let stopped: Void = stoppingHost.shutdownAndWait()
                            await account.logout()
                            await stopped
                        }
                    }
                    .buttonStyle(.bordered)
                }
            }
        }
    }

    private var videoSize: CGSize? {
        guard let pixelBuffer = bridge.latestPixelBuffer else { return nil }
        return CGSize(width: CVPixelBufferGetWidth(pixelBuffer), height: CVPixelBufferGetHeight(pixelBuffer))
    }

    private var keyboardAvailable: Bool {
        if case .connected = bridge.state { return true }
        return false
    }

    private var sessionOperationsAvailable: Bool {
        guard bridge.capabilities != nil else { return false }
        return keyboardAvailable
    }

    private func selectQuality(_ preference: SessionVideoQualityPreference) {
        guard sessionOperationsAvailable, !operations.hasPending else { return }
        if bridge.sendSessionCommand(3, value: preference.rawValue) {
            operations.qualityQueued(preference)
        } else {
            operations.couldNotSend(bridge.lastError)
        }
    }

    private func selectDisplay(_ identifier: UInt32) {
        guard sessionOperationsAvailable, !operations.hasPending,
              bridge.remoteDisplays.contains(where: { $0.id == identifier }) else { return }
        if let context = bridge.captureInputContext() { bridge.releaseAll(context: context) }
        if bridge.sendSessionCommand(1, value: identifier) {
            operations.displayQueued(identifier)
        } else {
            operations.couldNotSend(bridge.lastError)
        }
    }

    private func displayTitle(_ display: SessionDisplay) -> String {
        let index = bridge.remoteDisplays.firstIndex(where: { $0.id == display.id }) ?? 0
        return "显示器 \(index + 1)\(display.primary ? " · 主屏" : "") · \(display.width) × \(display.height)"
    }

    private func sendSystemKey(_ code: UInt32, modifiers: Modifiers = []) {
        guard let context = bridge.captureInputContext() else { return }
        bridge.releaseAll(context: context)
        bridge.send(input: .key(code: code, pressed: true, modifiers: modifiers), context: context)
        bridge.send(input: .key(code: code, pressed: false, modifiers: modifiers), context: context)
    }

    private var cursorStreamID: UInt64? {
        if case let .connected(streamID) = bridge.state { return streamID }
        if bridge.state == .frozen || bridge.state == .recovering {
            return bridge.cursorOverlay?.streamID
        }
        return nil
    }

    private var statusText: String {
        deskLinkSessionStatusText(for: bridge.state)
    }

    private var videoPlaceholder: String {
        switch bridge.state {
        case .reconnecting: "网络暂时中断，DeskLink 正在重新连接"
        case .recovering: "正在等待新的关键帧"
        case .frozen: "远程画面暂时没有更新"
        default: "正在准备远程画面"
        }
    }

    private var sessionStatusColor: Color {
        switch bridge.state {
        case .connected: DeskLinkPalette.success
        case .frozen: DeskLinkPalette.warning
        default: DeskLinkPalette.info
        }
    }
}
