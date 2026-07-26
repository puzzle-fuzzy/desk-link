import DeskLinkAppleCore
import SwiftUI

enum IOSSessionPresentation {
    static func isActive(_ state: ConnectionState) -> Bool {
        switch state {
        case .connected, .reconnecting, .recovering, .frozen:
            true
        default:
            false
        }
    }

    static func statusText(_ state: ConnectionState) -> String {
        switch state {
        case .connected: "已连接"
        case .reconnecting: "正在重新连接"
        case .recovering: "正在恢复画面"
        case .frozen: "画面暂时冻结"
        case .pairing: "等待用户审批"
        case .connecting: "正在建立安全连接"
        case .idle: "准备连接"
        case .closed: "已断开"
        case .failed: "连接失败"
        }
    }
}

struct IOSSessionView: View {
    @ObservedObject var controller: ControllerBridge
    @State private var visibleVideoRect: CGRect = .zero
    @State private var touchMode: IOSTouchMode = .direct
    @State private var showingDiagnostics = false
    @StateObject private var keyboardInput: IOSKeyboardInput

    init(controller: ControllerBridge) {
        self.controller = controller
        _keyboardInput = StateObject(wrappedValue: IOSKeyboardInput(bridge: controller))
    }

    var body: some View {
        VStack(spacing: 0) {
            remoteCanvas
                .frame(maxWidth: .infinity, maxHeight: .infinity)

            sessionControls
        }
        .background(Color(uiColor: .systemGroupedBackground))
        .toolbar(.hidden, for: .tabBar)
        .sheet(isPresented: $showingDiagnostics) {
            NavigationStack {
                Form {
                    Section("视频") {
                        Text("分辨率：\(videoSizeText)")
                        Text("已接收帧：\(controller.metrics.receivedFrames)")
                        Text("丢弃帧：\(controller.metrics.droppedFrames)")
                        if let frameID = controller.metrics.lastFrameID {
                            Text("最新帧：\(frameID)")
                        }
                    }
                }
                .navigationTitle("诊断")
                .navigationBarTitleDisplayMode(.inline)
            }
            .presentationDetents([.medium])
        }
        .onDisappear {
            keyboardInput.resign()
            controller.releaseAll()
        }
    }

    private var remoteCanvas: some View {
        ZStack {
            Color.black
            IOSMetalVideoView(
                pixelBuffer: controller.latestPixelBuffer,
                videoSize: controller.videoSize,
                visibleVideoRect: $visibleVideoRect
            )
            IOSTouchInputView(
                bridge: controller,
                videoSize: controller.videoSize,
                visibleVideoRect: visibleVideoRect,
                mode: touchMode
            )
            .allowsHitTesting(isInputEnabled)

            if !hasVideoFrame {
                VStack(spacing: 10) {
                    Image(systemName: "rectangle.inset.filled")
                        .font(.system(size: 28, weight: .medium))
                        .foregroundStyle(.white.opacity(0.82))
                    Text(videoPlaceholder)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(.white)
                    Text("输入仍可使用，画面恢复后会自动显示")
                        .font(.caption)
                        .foregroundStyle(.white.opacity(0.68))
                        .multilineTextAlignment(.center)
                }
                .padding(.horizontal, 24)
                .allowsHitTesting(false)
            }

            VStack {
                HStack {
                    Label(statusText, systemImage: statusSymbol)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 11)
                        .padding(.vertical, 7)
                        .background(Color.black.opacity(0.68), in: Capsule())
                    Spacer()
                }
                .padding(.horizontal, 14)
                .padding(.top, 14)
                Spacer()
            }
            .allowsHitTesting(false)
        }
        .clipped()
        .overlay {
            IOSKeyboardInputView(input: keyboardInput)
                .frame(width: 1, height: 1)
                .opacity(0.01)
        }
    }

    private var sessionControls: some View {
        VStack(spacing: 10) {
            IOSSpecialKeyBar(keyboard: keyboardInput)

            Picker("输入模式", selection: $touchMode) {
                ForEach(IOSTouchMode.allCases) { mode in
                    Text(mode.rawValue).tag(mode)
                }
            }
            .pickerStyle(.segmented)
            .padding(.horizontal, 12)

            HStack(spacing: 8) {
                sessionAction(
                    title: "键盘",
                    accessibilityLabel: "打开键盘输入",
                    systemImage: "keyboard",
                    enabled: isInputEnabled
                ) {
                    keyboardInput.becomeFirstResponder()
                }
                sessionAction(
                    title: "诊断",
                    accessibilityLabel: "打开连接诊断",
                    systemImage: "gauge"
                ) {
                    showingDiagnostics = true
                }
                sessionAction(
                    title: "关键帧",
                    accessibilityLabel: "请求关键帧",
                    systemImage: "arrow.clockwise",
                    enabled: isInputEnabled
                ) {
                    controller.requestKeyframe()
                }
                sessionAction(
                    title: "断开",
                    accessibilityLabel: "断开连接",
                    systemImage: "xmark.circle",
                    tint: .red
                ) {
                    keyboardInput.resign()
                    controller.disconnect()
                }
            }
            .padding(.horizontal, 12)
            .padding(.bottom, 10)
        }
        .background(.bar)
        .overlay(alignment: .top) { Divider() }
    }

    private func sessionAction(
        title: String,
        accessibilityLabel: String,
        systemImage: String,
        tint: Color? = nil,
        enabled: Bool = true,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            VStack(spacing: 4) {
                Image(systemName: systemImage)
                    .font(.body.weight(.semibold))
                Text(title)
                    .font(.caption.weight(.semibold))
                    .lineLimit(1)
                    .minimumScaleFactor(0.8)
            }
            .frame(maxWidth: .infinity, minHeight: 48)
        }
        .buttonStyle(.bordered)
        .tint(tint)
        .disabled(!enabled)
        .accessibilityLabel(accessibilityLabel)
    }

    private var isInputEnabled: Bool {
        if case .connected = controller.state { return true }
        return false
    }

    private var hasVideoFrame: Bool {
        controller.latestPixelBuffer != nil
    }

    private var statusText: String {
        IOSSessionPresentation.statusText(controller.state)
    }

    private var statusSymbol: String {
        switch controller.state {
        case .connected: "checkmark.circle.fill"
        case .reconnecting, .recovering: "arrow.triangle.2.circlepath"
        case .frozen: "pause.circle.fill"
        case .failed: "exclamationmark.triangle.fill"
        default: "circle"
        }
    }

    private var videoPlaceholder: String {
        switch controller.state {
        case .reconnecting: "网络暂时中断，正在重新连接"
        case .recovering: "正在等待新的关键帧"
        case .frozen: "远程画面暂时没有更新"
        default: "正在接收远程画面"
        }
    }

    private var videoSizeText: String {
        guard let videoSize = controller.videoSize else { return "等待视频配置" }
        return "\(Int(videoSize.width)) × \(Int(videoSize.height))"
    }
}
