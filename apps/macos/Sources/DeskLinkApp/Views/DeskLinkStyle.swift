import AppKit
import SwiftUI
import DeskLinkAppleCore

enum DeskLinkSection: String, CaseIterable, Identifiable {
    case connect = "连接设备"
    case share = "共享此设备"
    case settings = "设置与诊断"

    var systemImage: String {
        switch self {
        case .connect: "desktopcomputer.and.arrow.right"
        case .share: "display"
        case .settings: "gearshape"
        }
    }

    var id: Self { self }
}

enum DeskLinkPalette {
    // Use AppKit semantic colors so the content follows the user's macOS
    // appearance and accent color instead of shipping a second fixed theme.
    static var background: Color { Color(nsColor: .windowBackgroundColor) }
    static var surface: Color { Color(nsColor: .controlBackgroundColor) }
    static var subtle: Color { Color(nsColor: .underPageBackgroundColor) }
    static var quiet: Color { Color(nsColor: .controlBackgroundColor).opacity(0.72) }
    static var ink: Color { Color(nsColor: .labelColor) }
    static var secondaryInk: Color { Color(nsColor: .secondaryLabelColor) }
    static var mutedInk: Color { Color(nsColor: .tertiaryLabelColor) }
    static var border: Color { Color(nsColor: .separatorColor) }
    static var primary: Color { Color(nsColor: .controlAccentColor) }
    static var primaryPressed: Color { Color(nsColor: .controlAccentColor).opacity(0.78) }
    static var success: Color { Color(nsColor: .systemGreen) }
    static var successSurface: Color { Color(nsColor: .systemGreen).opacity(0.13) }
    static var info: Color { Color(nsColor: .systemBlue) }
    static var infoSurface: Color { Color(nsColor: .systemBlue).opacity(0.11) }
    static var warning: Color { Color(nsColor: .systemOrange) }
    static var warningSurface: Color { Color(nsColor: .systemOrange).opacity(0.13) }
    static var error: Color { Color(nsColor: .systemRed) }
    static var errorSurface: Color { Color(nsColor: .systemRed).opacity(0.12) }
}

struct DeskLinkShell: View {
    @ObservedObject var host: HostBridge
    @ObservedObject var controller: ControllerBridge
    @ObservedObject var account: AccountClient

    @State private var section: DeskLinkSection? = .connect
    @State private var isShowingHostStatus = false
    @StateObject private var connectionDraft = DeskLinkConnectionDraft()

    var body: some View {
        NavigationSplitView {
            List(selection: $section) {
                Section {
                    ForEach(DeskLinkSection.allCases) { item in
                        Label(item.rawValue, systemImage: item.systemImage)
                            .tag(item)
                            .padding(.vertical, 5)
                    }
                } header: {
                    HStack(spacing: 9) {
                        DeskLinkMark()
                        Text("DeskLink")
                            .font(.system(size: 18, weight: .semibold))
                            .foregroundStyle(DeskLinkPalette.ink)
                    }
                    .padding(.vertical, 12)
                }
            }
            .listStyle(.sidebar)
            .accessibilityLabel("DeskLink 主导航")
            .navigationSplitViewColumnWidth(min: 190, ideal: 205, max: 250)
            .safeAreaInset(edge: .bottom) {
                VStack(alignment: .leading, spacing: 5) {
                    Label(hostStatus.title, systemImage: hostStatus.systemImage)
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(hostStatus.tone == .attention ? DeskLinkPalette.warning : DeskLinkPalette.secondaryInk)
                    Text(accountSummary)
                        .font(.system(size: 11))
                        .foregroundStyle(DeskLinkPalette.mutedInk)
                        .lineLimit(1)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(16)
            }
        } detail: {
            Group {
                switch section ?? .connect {
                case .connect:
                    ControllerHomeView(bridge: controller, draft: connectionDraft)
                case .share:
                    HostHomeView(bridge: host, page: .connection)
                case .settings:
                    HostHomeView(bridge: host, page: .overview, controller: controller, account: account)
                }
            }
            .navigationTitle((section ?? .connect).rawValue)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .navigationSplitViewStyle(.balanced)
        .frame(minWidth: 980, minHeight: 620)
        .background(DeskLinkPalette.background)
        .tint(DeskLinkPalette.primary)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    isShowingHostStatus = true
                } label: {
                    Label("本机共享", systemImage: hostStatus.systemImage)
                }
                .help("查看本机共享状态")
                .popover(isPresented: $isShowingHostStatus) {
                    DeskLinkHostStatusPopover(
                        host: host,
                        openSettings: {
                            isShowingHostStatus = false
                            section = .settings
                        },
                        openSharing: {
                            isShowingHostStatus = false
                            section = .share
                        }
                    )
                }
            }
        }
        .onChange(of: controller.state) { state in connectionDraft.observe(state) }
    }

    private var hostStatus: DeskLinkHostStatus {
        deskLinkHostStatus(
            for: host.state,
            permissions: host.permissions,
            hasPendingApproval: host.pendingApproval != nil,
            lastError: host.lastError,
            sharingStatus: host.sharingStatus
        )
    }

    private var accountSummary: String {
        switch account.state {
        case let .signedIn(user): user.email
        case let .offline(user): user?.email ?? "离线模式 · 未登录"
        case .skipped: "本机模式 · 未登录"
        case .loading: "正在加载账号"
        case .signedOut: "未登录"
        }
    }
}

struct DeskLinkMark: View {
    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 7)
                .fill(DeskLinkPalette.ink)
                .frame(width: 28, height: 28)
            Circle()
                .stroke(DeskLinkPalette.primary.opacity(0.65), lineWidth: 1)
                .frame(width: 18, height: 18)
            Circle()
                .fill(DeskLinkPalette.primary)
                .frame(width: 12, height: 12)
            Circle()
                .fill(Color.white)
                .frame(width: 4, height: 4)
        }
        .accessibilityHidden(true)
    }
}

struct DeskLinkPanel<Content: View>: View {
    let background: Color
    let content: () -> Content

    init(background: Color = DeskLinkPalette.subtle, @ViewBuilder content: @escaping () -> Content) {
        self.background = background
        self.content = content
    }

    var body: some View {
        content()
            .padding(20)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(background, in: RoundedRectangle(cornerRadius: 8))
            .overlay {
                RoundedRectangle(cornerRadius: 8)
                    .stroke(DeskLinkPalette.border, lineWidth: 1)
            }
    }
}

struct DeskLinkPrimaryButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 13, weight: .semibold))
            .foregroundStyle(Color(nsColor: .alternateSelectedControlTextColor))
            .padding(.horizontal, 16)
            .frame(minHeight: 32)
            .background(
                configuration.isPressed ? DeskLinkPalette.primaryPressed : DeskLinkPalette.primary,
                in: RoundedRectangle(cornerRadius: 5)
            )
            .opacity(isEnabled ? 1 : 0.48)
    }
}

struct DeskLinkSecondaryButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 13, weight: .semibold))
            .foregroundStyle(DeskLinkPalette.ink)
            .padding(.horizontal, 14)
            .frame(minHeight: 32)
            .background(
                configuration.isPressed ? DeskLinkPalette.quiet : DeskLinkPalette.surface,
                in: RoundedRectangle(cornerRadius: 5)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 5)
                    .stroke(DeskLinkPalette.border, lineWidth: 1)
            }
            .opacity(isEnabled ? 1 : 0.48)
    }
}

struct DeskLinkStatusLight: View {
    let color: Color

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: 8, height: 8)
            .overlay { Circle().stroke(color.opacity(0.25), lineWidth: 5) }
            .accessibilityHidden(true)
    }
}

struct DeskLinkErrorView: View {
    let message: String

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "exclamationmark.circle.fill")
                .foregroundStyle(DeskLinkPalette.error)
            Text(deskLinkChineseError(message))
                .font(.system(size: 13))
                .foregroundStyle(DeskLinkPalette.ink)
                .textSelection(.enabled)
            Spacer(minLength: 0)
        }
        .padding(14)
        .background(DeskLinkPalette.errorSurface, in: RoundedRectangle(cornerRadius: 8))
    }
}

func deskLinkChineseError(_ message: String) -> String {
    if message.unicodeScalars.contains(where: { $0.value >= 0x4E00 && $0.value <= 0x9FFF }) {
        return message
    }
    let value = message.lowercased()
    if value.contains("relay") {
        return "中继服务暂时不可用，请检查网络或中继配置后重试。"
    }
    if value.contains("occupied") || value.contains("already in use") {
        return "上一条连接正在释放，DeskLink 会自动恢复。"
    }
    if value.contains("permission") || value.contains("screen recording") || value.contains("accessibility") {
        return "macOS 权限尚未就绪，请检查屏幕录制与辅助功能权限。"
    }
    if value.contains("secure") || value.contains("auth") || value.contains("secret") || value.contains("key") {
        return "安全连接校验失败，请从另一台设备重新创建连接码。"
    }
    if value.contains("video") || value.contains("decoder") || value.contains("encoder") {
        return "远程画面暂时不可用，请等待恢复或请求关键帧。"
    }
    return "连接出现问题，请检查两台设备和网络后重试。"
}
