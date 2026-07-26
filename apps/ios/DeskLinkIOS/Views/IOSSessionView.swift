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

                VStack {
                    HStack {
                        Text(IOSSessionPresentation.statusText(controller.state))
                            .font(.caption.weight(.semibold))
                            .padding(.horizontal, 10)
                            .padding(.vertical, 6)
                            .background(.ultraThinMaterial, in: Capsule())
                        Spacer()
                    }
                    .padding(12)
                    Spacer()
                }
            }
            .clipped()
            .overlay(alignment: .bottom) {
                IOSSpecialKeyBar(keyboard: keyboardInput)
                    .background(.ultraThinMaterial)
            }
            .overlay {
                IOSKeyboardInputView(input: keyboardInput)
                    .frame(width: 1, height: 1)
                    .opacity(0.01)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)

            VStack(spacing: 10) {
                Picker("输入模式", selection: $touchMode) {
                    ForEach(IOSTouchMode.allCases) { mode in
                        Text(mode.rawValue).tag(mode)
                    }
                }
                .pickerStyle(.segmented)

                HStack {
                    Button {
                        keyboardInput.becomeFirstResponder()
                    } label: {
                        Label("键盘", systemImage: "keyboard")
                    }
                    .buttonStyle(.bordered)

                    Button {
                        showingDiagnostics = true
                    } label: {
                        Label("诊断", systemImage: "gauge")
                    }
                    .buttonStyle(.bordered)

                    Spacer()

                    Button("请求关键帧") { controller.requestKeyframe() }
                        .buttonStyle(.bordered)
                    Button("断开连接", role: .destructive) {
                        keyboardInput.resign()
                        controller.disconnect()
                    }
                    .buttonStyle(.borderedProminent)
                }
            }
            .padding(12)
            .background(.bar)
        }
        .navigationTitle("远程会话")
        .navigationBarTitleDisplayMode(.inline)
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

    private var isInputEnabled: Bool {
        if case .connected = controller.state { return true }
        return false
    }

    private var videoSizeText: String {
        guard let videoSize = controller.videoSize else { return "等待视频配置" }
        return "\(Int(videoSize.width)) × \(Int(videoSize.height))"
    }
}
