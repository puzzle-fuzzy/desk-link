import SwiftUI

enum DeskLinkHostStatusTone: Equatable {
    case ready
    case attention
    case idle
    case working
}

struct DeskLinkHostStatus: Equatable {
    let title: String
    let detail: String
    let tone: DeskLinkHostStatusTone

    var systemImage: String {
        switch tone {
        case .ready: "checkmark.circle"
        case .attention: "exclamationmark.circle"
        case .idle: "circle"
        case .working: "arrow.triangle.2.circlepath"
        }
    }
}

func deskLinkHostStatus(for state: HostState, lastError: String?) -> DeskLinkHostStatus {
    if lastError != nil {
        return DeskLinkHostStatus(
            title: "需要处理",
            detail: "打开设置检查本机共享权限",
            tone: .attention
        )
    }

    switch state {
    case .idle, .closed:
        return DeskLinkHostStatus(
            title: "未开启共享",
            detail: "需要共享这台 Mac 时生成连接码",
            tone: .idle
        )
    case .connecting, .waitingForApproval, .negotiating, .stopping:
        return DeskLinkHostStatus(
            title: "连接中",
            detail: "正在准备本机共享",
            tone: .working
        )
    case .connected:
        return DeskLinkHostStatus(
            title: "本机可被连接",
            detail: "共享已准备好，等待远程控制请求",
            tone: .ready
        )
    case .failed:
        return DeskLinkHostStatus(
            title: "需要处理",
            detail: "打开设置查看共享错误",
            tone: .attention
        )
    }
}

struct DeskLinkHostStatusPopover: View {
    @ObservedObject var host: HostBridge
    @ObservedObject var controller: ControllerBridge

    var body: some View {
        let status = deskLinkHostStatus(for: host.state, lastError: host.lastError)

        VStack(alignment: .leading, spacing: 8) {
            Label(status.title, systemImage: status.systemImage)
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(color(for: status.tone))
            Text(status.detail)
                .font(.system(size: 12))
                .foregroundStyle(DeskLinkPalette.secondaryInk)

            if controller.lastError != nil {
                Divider()
                Text("另一台设备的连接需要检查")
                    .font(.system(size: 12))
                    .foregroundStyle(DeskLinkPalette.warning)
            }
        }
        .padding(16)
        .frame(width: 260, alignment: .leading)
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
