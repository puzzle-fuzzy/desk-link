import SwiftUI

enum DeskLinkSection: String, CaseIterable, Identifiable {
    case connect = "连接设备"
    case share = "共享此设备"
    case devices = "已批准设备"
    case settings = "设置 / 诊断"

    var id: Self { self }
}

enum DeskLinkPalette {
    static let background = Color(red: 0.956, green: 0.951, blue: 0.946)
    static let surface = Color.white
    static let subtle = Color(red: 0.976, green: 0.972, blue: 0.968)
    static let quiet = Color(red: 0.941, green: 0.933, blue: 0.926)
    static let ink = Color(red: 0.170, green: 0.145, blue: 0.135)
    static let secondaryInk = Color(red: 0.350, green: 0.315, blue: 0.300)
    static let mutedInk = Color(red: 0.455, green: 0.415, blue: 0.395)
    static let border = Color(red: 0.835, green: 0.815, blue: 0.800)
    static let primary = Color(red: 0.720, green: 0.255, blue: 0.125)
    static let primaryPressed = Color(red: 0.590, green: 0.190, blue: 0.090)
    static let success = Color(red: 0.160, green: 0.535, blue: 0.295)
    static let successSurface = Color(red: 0.925, green: 0.975, blue: 0.940)
    static let info = Color(red: 0.160, green: 0.440, blue: 0.610)
    static let infoSurface = Color(red: 0.925, green: 0.965, blue: 0.985)
    static let warning = Color(red: 0.690, green: 0.425, blue: 0.080)
    static let warningSurface = Color(red: 0.985, green: 0.960, blue: 0.900)
    static let error = Color(red: 0.690, green: 0.145, blue: 0.120)
    static let errorSurface = Color(red: 0.990, green: 0.935, blue: 0.925)
}

struct DeskLinkShell<Content: View>: View {
    @Binding var selection: DeskLinkSection
    @ObservedObject var host: HostBridge
    @ObservedObject var controller: ControllerBridge
    let content: () -> Content

    @State private var isShowingHostStatus = false

    init(
        selection: Binding<DeskLinkSection>,
        host: HostBridge,
        controller: ControllerBridge,
        @ViewBuilder content: @escaping () -> Content
    ) {
        _selection = selection
        self.host = host
        self.controller = controller
        self.content = content
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 12) {
                DeskLinkMark()
                HStack(alignment: .firstTextBaseline, spacing: 9) {
                    Text("DeskLink")
                        .font(.system(size: 17, weight: .semibold))
                        .foregroundStyle(DeskLinkPalette.ink)
                    Text("macOS 远程桌面")
                        .font(.system(size: 12))
                        .foregroundStyle(DeskLinkPalette.mutedInk)
                }
                Spacer()
                Button {
                    isShowingHostStatus = true
                } label: {
                    let status = deskLinkHostStatus(for: host.state, lastError: host.lastError)
                    Label(status.title, systemImage: status.systemImage)
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(color(for: status.tone))
                }
                .buttonStyle(.plain)
                .popover(isPresented: $isShowingHostStatus) {
                    DeskLinkHostStatusPopover(
                        host: host,
                        openSettings: { selection = .settings },
                        openSharing: { selection = .share }
                    )
                }
            }
            .padding(.horizontal, 28)
            .frame(height: 65)
            .background(DeskLinkPalette.surface)

            Rectangle().fill(DeskLinkPalette.border).frame(height: 1)

            HStack(spacing: 26) {
                ForEach(DeskLinkSection.allCases) { section in
                    Button {
                        selection = section
                    } label: {
                        Text(section.rawValue)
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundStyle(
                                selection == section ? DeskLinkPalette.ink : DeskLinkPalette.mutedInk
                            )
                            .padding(.horizontal, 2)
                            .frame(height: 47)
                            .overlay(alignment: .bottom) {
                                Rectangle()
                                    .fill(selection == section ? DeskLinkPalette.primary : Color.clear)
                                    .frame(height: 2)
                            }
                    }
                    .buttonStyle(.plain)
                    .accessibilityAddTraits(selection == section ? .isSelected : [])
                }
                Spacer()
            }
            .padding(.horizontal, 28)
            .background(DeskLinkPalette.subtle)

            Rectangle().fill(DeskLinkPalette.border).frame(height: 1)

            content()
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                .background(DeskLinkPalette.surface)
        }
        .frame(minWidth: 760, minHeight: 560)
        .background(DeskLinkPalette.surface)
    }

    private func color(for tone: DeskLinkHostStatusTone) -> Color {
        switch tone {
        case .ready: DeskLinkPalette.success
        case .attention: DeskLinkPalette.warning
        case .idle: DeskLinkPalette.mutedInk
        case .working: DeskLinkPalette.info
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
            .foregroundStyle(Color.white)
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
