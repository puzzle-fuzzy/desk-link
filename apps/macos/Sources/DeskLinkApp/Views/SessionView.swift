import AppKit
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
    @ObservedObject private var media: ControllerMediaBridge
    @StateObject private var presentation = SessionPresentation()
    @StateObject private var clipboard: MacClipboardController
    @StateObject private var audio = RemoteAudioPlayer()
    @StateObject private var files: MacFileTransferController
    @State private var diagnosticsExpanded = false
    @State private var modifierMode: ModifierMode = .automatic
    @State private var showsTextEntry = false
    @State private var textEntry = ""
    @State private var textEntryError: String?
    @State private var showsAudioControls = false
    @State private var showsTransfers = false

    init(bridge: ControllerBridge, host: HostBridge, account: AccountClient,
         clipboard: MacClipboardController = MacClipboardController(),
         files: MacFileTransferController = MacFileTransferController()) {
        self.bridge = bridge
        self.host = host
        self.account = account
        _media = ObservedObject(wrappedValue: bridge.media)
        _clipboard = StateObject(wrappedValue: clipboard)
        _files = StateObject(wrappedValue: files)
    }

    var body: some View {
        VStack(spacing: 0) {
            if !presentation.isFullscreen { sessionToolbar }
            GeometryReader { proxy in
                let viewport = SessionViewport(sourceSize: videoSize,
                                               viewportSize: proxy.size,
                                               backingScale: presentation.backingScale,
                                               scalingMode: presentation.scalingMode,
                                               panOffset: presentation.panOffset)
                ZStack(alignment: .top) {
                    MetalVideoView(frame: bridge.videoFrame, viewport: viewport,
                                   onFramePresented: { [bridge] token, _ in
                                       bridge.confirmVideoFramePresentation(token)
                                   })
                        .background(Color.black)
                    SessionInputView(bridge: bridge, viewport: viewport,
                                     interactionMode: presentation.interactionMode,
                                     acceptsRemoteInput: acceptsRemoteInput,
                                     exitFullscreen: { presentation.exitFullscreen() },
                                     onPan: { presentation.panOffset = viewport.panned(by: $0) },
                                     onPointerActivity: { point in
                                         if presentation.isFullscreen, point.y < 24 { presentation.revealToolbar() }
                                     }, modifierMode: modifierMode,
                                     inputContextID: bridge.inputContextID,
                                     onPasteClipboard: { clipboard.sendLocalText(paste: true) },
                                     onFocusLost: { bridge.cancelPendingPaste() })
                        .background(Color.clear)
                    cursorOverlay(viewport: viewport)
                    if bridge.latestPixelBuffer == nil || bridge.isAwaitingDisplayFrame {
                        VStack(spacing: 10) {
                            ProgressView().controlSize(.small)
                            Text(bridge.isAwaitingDisplayFrame ? "正在切换屏幕，等待新画面" : videoPlaceholder)
                                .font(.system(size: 13))
                                .foregroundStyle(Color.white.opacity(0.88))
                        }
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .allowsHitTesting(false)
                    }
                    if presentation.isFullscreen {
                        if presentation.toolbarIsVisible {
                            sessionToolbar
                                .padding(.horizontal, 18)
                                .padding(.top, 12)
                                .onHover { presentation.toolbarHovered = $0 }
                        }
                    }
                    if presentation.interactionMode == .browse {
                        VStack {
                            Spacer()
                            Text(presentation.scalingMode == .fit
                                 ? "浏览模式 · 键鼠不会发送到远端，切换 1:1 后可拖动画面"
                                 : "浏览模式 · 拖动或滚动浏览画面，键鼠不会发送到远端")
                                .font(.system(size: 12))
                                .padding(.horizontal, 14).padding(.vertical, 8)
                                .background(.ultraThinMaterial, in: Capsule())
                                .padding(.bottom, 16)
                        }
                        .allowsHitTesting(false)
                    }
                }
                .clipped()
            }
            if !presentation.isFullscreen { sessionFooter }
        }
        .frame(minWidth: 760, minHeight: 520)
        .background(SessionWindowReader { presentation.attach(window: $0) }.frame(width: 0, height: 0))
        .onAppear {
            presentation.releaseInput = { [weak bridge] in bridge?.releaseAll() }
            clipboard.attach(to: bridge)
            files.attach(to: media)
            media.onAudioStateChanged = { [weak audio] stream, enabled in audio?.setSession(streamID: stream, enabled: enabled) }
            media.onAudioFrame = { [weak audio] frame in
                audio?.receive(streamID: frame.streamID, sequence: frame.sequence, pcm: frame.pcm)
            }
            audio.setSession(streamID: media.streamID, enabled: media.audioEnabled)
        }
        .onChange(of: bridge.state) { _ in
            if !acceptsRemoteInput { bridge.releaseAll() }
        }
        .onDisappear {
            bridge.cancelPendingPaste()
            clipboard.detach()
            files.detach()
            media.onAudioFrame = nil; media.onAudioStateChanged = nil
            audio.stop()
            presentation.cleanup()
        }
        .onChange(of: audio.localError) { error in
            if error != nil { media.reportPlaybackFailure() }
        }
        .sheet(isPresented: $showsTextEntry) { textEntrySheet }
        .sheet(isPresented: $showsTransfers) {
            SessionTransferPanel(media: media, files: files) {
                bridge.cancelPendingPaste(); bridge.releaseAll()
            }
        }
    }

    private var sessionToolbar: some View {
        VStack(alignment: .leading, spacing: 8) {
          HStack(spacing: 10) {
            HStack(spacing: 7) {
                DeskLinkStatusLight(color: sessionStatusColor)
                Text(deskLinkSessionStatusText(for: bridge.state))
                    .font(.system(size: 12, weight: .semibold))
            }
            Spacer(minLength: 4)
            displayMenu
            qualityMenu
            Button { showsAudioControls.toggle() } label: {
                Image(systemName: media.audioEnabled && !audio.isMuted ? "speaker.wave.2" : "speaker.slash")
            }
            .accessibilityLabel("远端声音").help("远端系统声音与本机音量")
            .popover(isPresented: $showsAudioControls) { audioControls }
            Button {
                bridge.cancelPendingPaste(); bridge.releaseAll()
                showsTransfers = true
            } label: { Image(systemName: "arrow.up.arrow.down.square") }
            .accessibilityLabel("文件传输").help("发送或获取文件，查看传输进度")
            Button {
                presentation.toggleFullscreen()
            } label: {
                Image(systemName: presentation.isFullscreen ? "arrow.down.right.and.arrow.up.left" : "arrow.up.left.and.arrow.down.right")
            }
            .help(presentation.isFullscreen ? "退出全屏（Esc）" : "进入全屏（Esc 退出）")
            .accessibilityLabel(presentation.isFullscreen ? "退出全屏" : "进入全屏")
            Menu {
                Button("请求关键帧") { bridge.requestKeyframe() }
                if case .signedIn = account.state {
                    Button("退出登录") {
                        Task { @MainActor in
                            bridge.clearRemoteLinksForLogout()
                            let stoppingHost = host
                            async let stopped: Void = stoppingHost.shutdownAndWait()
                            await account.logout()
                            await stopped
                        }
                    }
                }
            } label: { Image(systemName: "ellipsis") }
            .menuStyle(.borderlessButton).fixedSize()
            .accessibilityLabel("会话操作")
            Button("断开连接") {
                bridge.releaseAll()
                _ = presentation.exitFullscreen()
                bridge.disconnect()
            }
            .buttonStyle(.borderedProminent)
          }
          HStack(spacing: 10) {
            Picker("画面缩放", selection: Binding(get: { presentation.scalingMode },
                                             set: { presentation.setScalingMode($0) })) {
                ForEach(SessionScalingMode.allCases) { Text($0.title).tag($0) }
            }
            .labelsHidden().frame(width: 126)
            .help("1:1 将一个远程像素映射为一个本机屏幕像素")
            Picker("操作模式", selection: Binding(get: { presentation.interactionMode },
                                             set: { presentation.setInteractionMode($0) })) {
                ForEach(SessionInteractionMode.allCases) { Text($0.title).tag($0) }
            }
            .labelsHidden()
            .pickerStyle(.segmented).frame(width: 158)
            Menu {
                Button("自动映射：Command → Ctrl") { modifierMode = .automatic }
                Button("原始映射：Command → Windows") { modifierMode = .raw }
            } label: {
                Text(modifierMode == .automatic ? "⌘ → Ctrl" : "⌘ → Windows")
            }
            .fixedSize().accessibilityLabel("快捷键映射")
            Spacer(minLength: 0)
            Menu {
                Button("发送本机文本到远端剪贴板") { clipboard.sendLocalText() }
                Button("取回远端文本到本机剪贴板") { clipboard.requestRemoteText() }
                Divider()
                Button("粘贴本机文本到远端") { clipboard.sendLocalText(paste: true) }
            } label: { Label("剪贴板", systemImage: "doc.on.clipboard") }
            .fixedSize().disabled(!canUseSessionControls || bridge.isClipboardBusy)
            Button("发送文字") {
                bridge.releaseAll()
                textEntryError = nil
                showsTextEntry = true
            }
            .disabled(!canUseSessionControls)
            shortcutMenu
          }
          if let message = featureMessage {
              HStack(spacing: 6) {
                  if bridge.pendingDisplayID != nil || bridge.pendingQuality != nil || bridge.isClipboardBusy {
                      ProgressView().controlSize(.mini)
                  }
                  Text(message).font(.system(size: 11)).lineLimit(2)
              }
              .foregroundStyle(featureHasError ? Color.red : DeskLinkPalette.mutedInk)
          }
        }
        .controlSize(.small)
        .padding(.horizontal, 16).padding(.vertical, 10)
        .background(.regularMaterial)
    }

    private var audioControls: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("远端系统声音").font(.headline)
            Button(media.audioEnabled || media.audioPending ? "关闭远端声音" : "开启远端声音") {
                _ = media.setAudioEnabled(!(media.audioEnabled || media.audioPending))
            }
            .disabled(!media.isConnected || (media.audioAvailable == false && !media.audioEnabled))
            if media.audioPending { ProgressView("等待远端确认…").controlSize(.small) }
            else if media.audioEnabled { Text(audio.isPlaying ? "正在播放" : "已开启，等待远端声音").font(.caption).foregroundStyle(.secondary) }
            HStack {
                Image(systemName: "speaker.fill")
                Slider(value: Binding(get: { audio.volume }, set: { audio.setVolume($0) }), in: 0...1)
                    .accessibilityLabel("远端声音音量")
                Text("\(Int(audio.volume * 100))%").monospacedDigit().frame(width: 36)
            }
            Toggle("本机静音", isOn: Binding(get: { audio.isMuted }, set: { audio.setMuted($0) }))
            if let error = media.audioError ?? audio.localError { Text(error).font(.caption).foregroundStyle(.red) }
            Text("播放对方电脑的系统声音。").font(.caption).foregroundStyle(.secondary)
        }
        .padding(18).frame(width: 280)
    }

    private var displayMenu: some View {
        Menu {
            ForEach(bridge.displays) { display in
                Button {
                    _ = bridge.selectDisplay(display.id)
                } label: {
                    if display.id == bridge.selectedDisplayID { Label(display.label, systemImage: "checkmark") }
                    else { Text(display.label) }
                }
            }
        } label: {
            Label(bridge.pendingDisplayID.map { "切换屏幕 \($0)…" }
                  ?? bridge.selectedDisplayID.map { "屏幕 \($0)" } ?? "等待屏幕列表", systemImage: "display.2")
        }
        .fixedSize().accessibilityLabel("屏幕切换")
        .disabled(bridge.displays.isEmpty || bridge.pendingDisplayID != nil || bridge.pendingQuality != nil || !isConnected)
    }

    private var qualityMenu: some View {
        Menu {
            ForEach(VideoQualityPreference.allCases) { quality in
                Button { _ = bridge.chooseQuality(quality) } label: {
                    if quality == bridge.qualityPreference { Label(quality.title, systemImage: "checkmark") }
                    else { Text(quality.title) }
                }
            }
        } label: {
            Text("画质：" + (bridge.pendingQuality.map { $0.title + "…" } ?? qualityLabel))
        }
        .fixedSize().accessibilityLabel("画质选择")
        .disabled(!isConnected || bridge.pendingQuality != nil || bridge.isAwaitingDisplayFrame)
    }

    private var qualityLabel: String {
        if bridge.qualityPreference == .automatic, let applied = bridge.appliedQualityPreset {
            return "自动（\(applied.title)）"
        }
        return bridge.qualityPreference.title
    }

    private var shortcutMenu: some View {
        Menu {
            Button("Windows 键") { sendShortcut(code: DeskLinkKeyCode.meta) }
            Button("Alt + Tab") { sendShortcut(code: DeskLinkKeyCode.tab, modifiers: .option) }
            Button("任务管理器（Ctrl + Shift + Esc）") { sendShortcut(code: DeskLinkKeyCode.escape, modifiers: [.control, .shift]) }
        } label: { Text("组合键") }
        .fixedSize().disabled(!canUseSessionControls)
    }

    private func sendShortcut(code: UInt32, modifiers: Modifiers = []) {
        guard canUseSessionControls else { return }
        bridge.releaseAll()
        bridge.send(input: .key(code: code, pressed: true, modifiers: modifiers))
        bridge.send(input: .key(code: code, pressed: false, modifiers: modifiers))
        bridge.releaseAll()
    }

    private var textEntrySheet: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("发送文字到远端").font(.headline)
            Text("支持中文和多行文字。请先将远端光标放到需要输入的位置。")
                .font(.subheadline).foregroundStyle(.secondary)
            TextEditor(text: $textEntry).font(.body).frame(minHeight: 150)
                .border(DeskLinkPalette.border).accessibilityLabel("要发送的文字")
            HStack {
                Text("\(textEntry.utf8.count) / \(ControllerBridge.committedTextMaximumBytes) 字节")
                    .font(.caption).foregroundStyle(.secondary)
                Spacer()
                Button("取消") { showsTextEntry = false }.keyboardShortcut(.cancelAction)
                Button("发送") {
                    if bridge.sendText(textEntry) { textEntry = ""; showsTextEntry = false }
                    else { textEntryError = "文字未能发送，请检查连接与输入位置后重试。" }
                }
                .buttonStyle(.borderedProminent)
                .disabled(!canUseSessionControls || textEntry.isEmpty || textEntry.utf8.count > ControllerBridge.committedTextMaximumBytes)
            }
            if let textEntryError { Text(textEntryError).font(.caption).foregroundStyle(.red) }
        }
        .padding(24).frame(width: 500)
    }

    private var isConnected: Bool { if case .connected = bridge.state { true } else { false } }
    private var canUseSessionControls: Bool { acceptsRemoteInput && presentation.interactionMode == .control }
    private var featureHasError: Bool {
        bridge.displayError != nil || bridge.qualityError != nil || bridge.clipboardError != nil || clipboard.localError != nil
    }
    private var featureMessage: String? {
        if let error = bridge.displayError ?? bridge.qualityError ?? clipboard.localError ?? bridge.clipboardError { return error }
        if let display = bridge.pendingDisplayID { return "正在切换到屏幕 \(display)，确认新画面前暂停远端输入。" }
        if let quality = bridge.pendingQuality { return "正在应用\(quality.title)画质…" }
        if bridge.isClipboardBusy { return "正在传送剪贴板文本…" }
        return clipboard.localStatus ?? bridge.clipboardStatus
    }

    private var sessionFooter: some View {
        VStack(spacing: 0) {
            Rectangle().fill(DeskLinkPalette.border).frame(height: 1)
            HStack(spacing: 12) {
                Button {
                    diagnosticsExpanded.toggle()
                } label: {
                    Label("会话诊断", systemImage: diagnosticsExpanded ? "chevron.down" : "chevron.right")
                }.buttonStyle(.plain)
                if diagnosticsExpanded {
                    Text("已接收 \(bridge.metrics.receivedFrames) 帧 · 已丢弃 \(bridge.metrics.droppedFrames) 帧")
                }
                Spacer()
                Text(deskLinkSessionSafetyCopy)
            }
            .font(.system(size: 11)).foregroundStyle(DeskLinkPalette.mutedInk)
            .padding(.horizontal, 16).frame(height: 34)
            .background(DeskLinkPalette.subtle)
        }
    }

    @ViewBuilder
    private func cursorOverlay(viewport: SessionViewport) -> some View {
        if bridge.latestPixelBuffer != nil,
           let overlay = bridge.cursorOverlay,
           let cursor = RemoteCursor(overlay: overlay), cursor.isVisible {
            let point = viewport.cursorPoint(normalized: cursor.normalizedPosition)
            Image(nsImage: NSCursor.arrow.image)
                .resizable()
                .frame(width: NSCursor.arrow.image.size.width, height: NSCursor.arrow.image.size.height)
                .position(x: point.x + NSCursor.arrow.image.size.width / 2 - NSCursor.arrow.hotSpot.x,
                          y: point.y + NSCursor.arrow.image.size.height / 2 - NSCursor.arrow.hotSpot.y)
                .allowsHitTesting(false)
                .accessibilityHidden(true)
        }
    }

    private var videoSize: CGSize {
        if let pixelBuffer = bridge.latestPixelBuffer {
            return CGSize(width: CVPixelBufferGetWidth(pixelBuffer), height: CVPixelBufferGetHeight(pixelBuffer))
        }
        return bridge.videoSize ?? .zero
    }

    private var acceptsRemoteInput: Bool {
        guard bridge.latestPixelBuffer != nil, bridge.canSendRemoteInput else { return false }
        switch bridge.state {
        case .connected, .recovering, .frozen: return true
        default: return false
        }
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
