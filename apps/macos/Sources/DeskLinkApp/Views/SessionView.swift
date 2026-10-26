import CoreVideo
import DeskLinkAppleCore
import SwiftUI

let deskLinkSessionSafetyCopy = "退出窗口前，DeskLink 会释放所有按键与鼠标状态。"

func deskLinkSessionStatusText(for state: ConnectionState) -> String {
    deskLinkConnectionStatus(for: state).title
}

struct SessionView: View {
    @ObservedObject var bridge: ControllerBridge
    @ObservedObject var host: HostBridge
    @ObservedObject var account: AccountClient
    @State private var modifierMode = ModifierMode.automatic

    var body: some View {
        VStack(spacing: 0) {
            ZStack {
                MetalVideoView(pixelBuffer: bridge.latestPixelBuffer)
                    .background(Color.black)
                SessionInputView(bridge: bridge, videoSize: videoSize, modifierMode: modifierMode)
                    .background(Color.clear)
                GeometryReader { proxy in
                    RemoteCursorPositionLayer(
                        overlay: bridge.cursorOverlay,
                        activeStreamID: cursorStreamID,
                        videoRect: VideoGeometry.aspectFit(
                            source: videoSize ?? .zero,
                            in: CGRect(origin: .zero, size: proxy.size)
                        )
                    )
                }
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
        .onDisappear { bridge.releaseAll() }
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
                            bridge.send(input: .character(114, pressed: true, modifiers: .meta))
                            bridge.send(input: .character(114, pressed: false, modifiers: .meta))
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
            if case .signedIn = account.state {
                ToolbarItem(placement: .primaryAction) {
                    Button("退出登录") {
                        Task { @MainActor in
                            bridge.clearRemoteLinksForLogout()
                            await host.shutdownAndWait()
                            await account.logout()
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

    private func sendSystemKey(_ code: UInt32, modifiers: Modifiers = []) {
        bridge.releaseAll()
        bridge.send(input: .key(code: code, pressed: true, modifiers: modifiers))
        bridge.send(input: .key(code: code, pressed: false, modifiers: modifiers))
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
