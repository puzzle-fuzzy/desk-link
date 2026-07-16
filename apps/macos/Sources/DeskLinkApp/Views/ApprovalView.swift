import SwiftUI

struct ApprovalView: View {
    @ObservedObject var bridge: HostBridge
    let approval: HostApproval

    var body: some View {
        DeskLinkPanel(background: DeskLinkPalette.warningSurface) {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .top, spacing: 10) {
                    Image(systemName: "person.crop.circle.badge.questionmark")
                        .font(.system(size: 21))
                        .foregroundStyle(DeskLinkPalette.warning)
                    VStack(alignment: .leading, spacing: 4) {
                        Text("允许这台设备控制此 Mac？")
                            .font(.system(size: 16, weight: .semibold))
                            .foregroundStyle(DeskLinkPalette.ink)
                        Text("批准后，对方可以查看屏幕并发送键盘与鼠标输入。请先核对设备身份。")
                            .font(.system(size: 12))
                            .foregroundStyle(DeskLinkPalette.secondaryInk)
                    }
                }

                VStack(alignment: .leading, spacing: 6) {
                    Text("设备 ID")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(DeskLinkPalette.mutedInk)
                    Text(approval.deviceIDText)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(DeskLinkPalette.ink)
                        .textSelection(.enabled)
                    Text("安全指纹")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(DeskLinkPalette.mutedInk)
                        .padding(.top, 2)
                    Text(approval.fingerprint)
                        .font(.system(size: 12, design: .monospaced))
                        .foregroundStyle(DeskLinkPalette.ink)
                        .textSelection(.enabled)
                }

                HStack {
                    if let expiresAt = bridge.pairingInvite?.expiresAt {
                        Text("连接码有效期至 \(expiresAt.formatted(date: .omitted, time: .shortened))")
                            .font(.system(size: 11))
                            .foregroundStyle(DeskLinkPalette.mutedInk)
                    }
                    Spacer()
                    Button("拒绝连接") { bridge.reject() }
                        .buttonStyle(DeskLinkSecondaryButtonStyle())
                        .keyboardShortcut(.cancelAction)
                    Button("批准设备") { bridge.approve() }
                        .buttonStyle(DeskLinkPrimaryButtonStyle())
                        .disabled(!bridge.canApprove)
                }
                if !bridge.canApprove {
                    Text("批准前需要允许屏幕录制与辅助功能。")
                        .font(.system(size: 11))
                        .foregroundStyle(DeskLinkPalette.warning)
                }
            }
        }
    }
}
