import SwiftUI

struct DiagnosticsView: View {
    @ObservedObject var bridge: ControllerBridge

    var body: some View {
        DeskLinkPanel {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .firstTextBaseline) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text("连接诊断")
                            .font(.system(size: 16, weight: .semibold))
                            .foregroundStyle(DeskLinkPalette.ink)
                        Text("这里仅显示运行状态，不会显示连接密钥。")
                            .font(.system(size: 12))
                            .foregroundStyle(DeskLinkPalette.mutedInk)
                    }
                    Spacer()
                    Button("请求关键帧") { bridge.requestKeyframe() }
                        .buttonStyle(DeskLinkSecondaryButtonStyle())
                        .disabled(streamID == nil)
                }
                HStack(spacing: 0) {
                    diagnosticValue("状态", value: stateText)
                    Divider()
                    diagnosticValue("已接收画面", value: String(bridge.metrics.receivedFrames))
                    Divider()
                    diagnosticValue("已丢弃画面", value: String(bridge.metrics.droppedFrames))
                    Divider()
                    diagnosticValue("视频流", value: streamID.map(String.init) ?? "未连接")
                }
                .frame(minHeight: 54)
            }
        }
    }

    private func diagnosticValue(_ title: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(.system(size: 11))
                .foregroundStyle(DeskLinkPalette.mutedInk)
            Text(value)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(DeskLinkPalette.ink)
                .lineLimit(1)
        }
        .padding(.horizontal, 14)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var streamID: UInt64? {
        if case let .connected(streamID) = bridge.state { return streamID }
        return nil
    }

    private var stateText: String {
        switch bridge.state {
        case .idle: "空闲"
        case .pairing: "等待批准"
        case .connecting: "正在连接"
        case .connected: "已连接"
        case .reconnecting: "正在恢复"
        case .recovering: "恢复画面"
        case .frozen: "画面暂停"
        case .closed: "已断开"
        case .failed: "连接失败"
        }
    }
}
