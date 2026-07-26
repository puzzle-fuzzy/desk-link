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
    @State private var isMoreExpanded = false
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
            isMoreExpanded = false
            keyboard.resign()
        }
    }

    private var controlDock: some View {
        VStack(alignment: .trailing, spacing: 10) {
            if isMoreExpanded {
                morePanel
                    .transition(.scale(scale: 0.92, anchor: .bottomTrailing).combined(with: .opacity))
            }

            HStack(spacing: 10) {
                if isMoreExpanded {
                    keyboardControl
                }
                moreControl
                orientationControl
            }
        }
        .animation(.easeInOut(duration: 0.18), value: isMoreExpanded)
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

    private var moreControl: some View {
        sessionControlButton(
            systemName: isMoreExpanded ? "xmark" : "ellipsis",
            accessibilityLabel: isMoreExpanded ? "收起更多控制" : "更多控制",
            accessibilityHint: "显示键盘、快捷键和连接操作"
        ) {
            isMoreExpanded.toggle()
        }
    }

    private func sessionControlButton(
        systemName: String,
        accessibilityLabel: String,
        accessibilityHint: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(.white)
                .frame(width: 48, height: 48)
                .background(Color.black.opacity(0.72), in: Circle())
                .overlay {
                    Circle()
                        .stroke(Color.white.opacity(0.24), lineWidth: 1)
                }
        }
        .buttonStyle(.plain)
        .contentShape(Circle())
        .accessibilityLabel(accessibilityLabel)
        .accessibilityHint(accessibilityHint)
        .accessibilityIdentifier(accessibilityIdentifier(for: accessibilityLabel))
    }

    private func accessibilityIdentifier(for label: String) -> String {
        switch label {
        case "切换横屏和竖屏": "session-orientation"
        case "显示键盘", "收起键盘": "session-keyboard"
        case "更多控制", "收起更多控制": "session-more"
        default: "session-control"
        }
    }

    private var morePanel: some View {
        VStack(alignment: .trailing, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "keyboard.badge.ellipsis")
                Text("快捷控制")
                    .font(.subheadline.weight(.semibold))
            }
            .foregroundStyle(.white)

            IOSSpecialKeyBar(keyboard: keyboard)
                .frame(maxWidth: 360)
                .background(Color.white.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))

            HStack(spacing: 8) {
                Button {
                    controller.requestKeyframe()
                } label: {
                    Label("刷新画面", systemImage: "arrow.clockwise")
                        .font(.caption.weight(.medium))
                }
                .buttonStyle(.bordered)
                .tint(.white)

                Button("断开连接", role: .destructive) {
                    controller.disconnect()
                }
                .font(.caption.weight(.medium))
                .buttonStyle(.bordered)
                .tint(.red)
            }
        }
        .padding(12)
        .background(Color.black.opacity(0.78), in: RoundedRectangle(cornerRadius: 16))
        .overlay {
            RoundedRectangle(cornerRadius: 16)
                .stroke(Color.white.opacity(0.2), lineWidth: 1)
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
                mode: .direct
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
