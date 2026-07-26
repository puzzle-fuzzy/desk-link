import DeskLinkAppleCore
import SwiftUI
import UIKit

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
    @State private var viewport = IOSVideoViewport()
    @State private var touchMode: IOSTouchMode = .direct
    @StateObject private var keyboard: IOSKeyboardInput

    init(controller: ControllerBridge) {
        self.controller = controller
        _keyboard = StateObject(wrappedValue: IOSKeyboardInput(bridge: controller))
    }

    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            remoteCanvas
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .ignoresSafeArea()

            controlDock
                .padding(.trailing, 18)
                .padding(.bottom, 30)

            VStack {
                HStack {
                    Spacer()
                    disconnectControl
                }
                Spacer()
            }
            .padding(.top, 12)
            .padding(.trailing, 18)

            IOSKeyboardInputView(input: keyboard)
                .frame(width: 1, height: 1)
                .opacity(0.01)
                .allowsHitTesting(false)
        }
        .background(Color.black)
        .toolbar(.hidden, for: .tabBar)
        .onDisappear {
            keyboard.resign()
            controller.releaseAll()
        }
        .onChange(of: controller.state) { state in
            guard !IOSSessionPresentation.isActive(state) else { return }
            viewport.reset()
            keyboard.resign()
        }
    }

    private var controlDock: some View {
        HStack(spacing: 10) {
            mouseModeControl
            keyboardControl
            orientationControl
        }
    }

    private var mouseModeControl: some View {
        Menu {
            Picker("鼠标模式", selection: $touchMode) {
                ForEach(IOSTouchMode.allCases) { mode in
                    Label(mode.rawValue, systemImage: mode.systemImage)
                        .tag(mode)
                }
            }
        } label: {
            sessionControlLabel(systemName: touchMode.systemImage)
        }
        .buttonStyle(.plain)
        .contentShape(Circle())
        .accessibilityLabel("鼠标模式")
        .accessibilityValue(touchMode.rawValue)
        .accessibilityHint("切换直接触控或轨迹板")
        .accessibilityIdentifier("session-mouse-mode")
    }

    private var orientationControl: some View {
        sessionControlButton(
            systemName: "rotate.right",
            accessibilityLabel: "切换横屏和竖屏",
            accessibilityHint: "固定在屏幕右下角"
        ) {
            IOSOrientationController.toggle()
        }
    }

    private var keyboardControl: some View {
        sessionControlButton(
            systemName: keyboard.isKeyboardVisible ? "keyboard.fill" : "keyboard",
            accessibilityLabel: keyboard.isKeyboardVisible ? "收起键盘" : "显示键盘",
            accessibilityHint: "显示或收起 iPhone 键盘"
        ) {
            if keyboard.isKeyboardVisible {
                keyboard.resign()
            } else {
                keyboard.becomeFirstResponder()
            }
        }
    }

    private var disconnectControl: some View {
        sessionControlButton(
            systemName: "rectangle.portrait.and.arrow.right",
            accessibilityLabel: "断开连接",
            accessibilityHint: "结束当前远程会话",
            background: Color.red.opacity(0.86)
        ) {
            controller.disconnect()
        }
    }

    private func sessionControlButton(
        systemName: String,
        accessibilityLabel: String,
        accessibilityHint: String,
        background: Color = Color.black.opacity(0.72),
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            sessionControlLabel(systemName: systemName, background: background)
        }
        .buttonStyle(.plain)
        .contentShape(Circle())
        .accessibilityLabel(accessibilityLabel)
        .accessibilityHint(accessibilityHint)
        .accessibilityIdentifier(accessibilityIdentifier(for: accessibilityLabel))
    }

    private func sessionControlLabel(
        systemName: String,
        background: Color = Color.black.opacity(0.72)
    ) -> some View {
        Image(systemName: systemName)
            .font(.system(size: 17, weight: .semibold))
            .foregroundStyle(.white)
            .frame(width: 48, height: 48)
            .background(background, in: Circle())
            .overlay {
                Circle()
                    .stroke(Color.white.opacity(0.24), lineWidth: 1)
            }
    }

    private func accessibilityIdentifier(for label: String) -> String {
        switch label {
        case "切换横屏和竖屏": "session-orientation"
        case "显示键盘", "收起键盘": "session-keyboard"
        case "断开连接": "session-disconnect"
        default: "session-control"
        }
    }

    private var remoteCanvas: some View {
        GeometryReader { proxy in
            ZStack {
                Color.black
                IOSMetalVideoView(
                    pixelBuffer: controller.latestPixelBuffer,
                    videoSize: controller.videoSize,
                    zoomScale: viewport.zoomScale,
                    panOffset: viewport.panOffset,
                    visibleVideoRect: $visibleVideoRect
                )
                IOSTouchInputView(
                    bridge: controller,
                    videoSize: controller.videoSize,
                    visibleVideoRect: visibleVideoRect,
                    mode: touchMode,
                    onPinchChanged: { factor, anchor in
                        viewport.pinch(
                            factor: factor,
                            anchor: anchor,
                            videoSize: controller.videoSize,
                            bounds: proxy.size
                        )
                    },
                    onFourFingerPan: { delta in
                        viewport.pan(
                            delta: delta,
                            videoSize: controller.videoSize,
                            bounds: proxy.size
                        )
                    },
                    onTrackpadPositionChanged: { position in
                        viewport.keepPointerVisible(
                            normalizedPosition: position,
                            videoSize: controller.videoSize,
                            bounds: proxy.size
                        )
                    }
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

                if shouldShowStatusBadge {
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
                        .padding(.top, 54)
                        Spacer()
                    }
                    .allowsHitTesting(false)
                }
            }
            .frame(width: proxy.size.width, height: proxy.size.height)
        }
        .clipped()
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

    private var shouldShowStatusBadge: Bool {
        if case .connected = controller.state { return false }
        return true
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

}

@MainActor
enum IOSOrientationController {
    static func toggle() {
        guard let windowScene = UIApplication.shared.connectedScenes
            .compactMap({ $0 as? UIWindowScene })
            .first
        else { return }

        let orientations: UIInterfaceOrientationMask = windowScene.interfaceOrientation.isPortrait
            ? [.landscapeLeft, .landscapeRight]
            : .portrait
        windowScene.requestGeometryUpdate(
            UIWindowScene.GeometryPreferences.iOS(interfaceOrientations: orientations)
        )
    }
}
