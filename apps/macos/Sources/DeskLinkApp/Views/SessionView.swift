import CoreVideo
import SwiftUI

let deskLinkSessionSafetyCopy = "退出窗口前，DeskLink 会释放所有按键与鼠标状态。"

func deskLinkSessionStatusText(for state: ConnectionState) -> String {
    deskLinkConnectionStatus(for: state).title
}

struct SessionView: View {
    @ObservedObject var bridge: ControllerBridge

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 12) {
                DeskLinkMark()
                VStack(alignment: .leading, spacing: 2) {
                    Text("DeskLink")
                        .font(.system(size: 15, weight: .semibold))
                        .foregroundStyle(DeskLinkPalette.ink)
                    Text("正在控制另一台设备")
                        .font(.system(size: 11))
                        .foregroundStyle(DeskLinkPalette.mutedInk)
                }
                Spacer()
                HStack(spacing: 8) {
                    DeskLinkStatusLight(color: sessionStatusColor)
                    Text(statusText)
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(DeskLinkPalette.secondaryInk)
                }
                Button("请求关键帧") { bridge.requestKeyframe() }
                    .buttonStyle(DeskLinkSecondaryButtonStyle())
                Button("断开连接") { bridge.disconnect() }
                    .buttonStyle(DeskLinkPrimaryButtonStyle())
            }
            .padding(.horizontal, 20)
            .frame(height: 60)
            .background(DeskLinkPalette.surface)

            Rectangle().fill(DeskLinkPalette.border).frame(height: 1)

            ZStack {
                MetalVideoView(pixelBuffer: bridge.latestPixelBuffer)
                    .background(Color.black)
                SessionInputView(bridge: bridge, videoSize: videoSize)
                    .background(Color.clear)
                if bridge.latestPixelBuffer == nil {
                    VStack(spacing: 10) {
                        ProgressView()
                            .controlSize(.small)
                        Text(videoPlaceholder)
                            .font(.system(size: 13))
                            .foregroundStyle(Color.white.opacity(0.88))
                    }
                    .allowsHitTesting(false)
                }
            }

            Rectangle().fill(DeskLinkPalette.border).frame(height: 1)

            HStack(alignment: .top) {
                DisclosureGroup("会话诊断") {
                    HStack {
                        Text("已接收 \(bridge.metrics.receivedFrames) 帧")
                        Text("已丢弃 \(bridge.metrics.droppedFrames) 帧")
                    }
                    .padding(.top, 4)
                }
                Spacer()
                Text(deskLinkSessionSafetyCopy)
            }
            .font(.system(size: 11))
            .foregroundStyle(DeskLinkPalette.mutedInk)
            .padding(.horizontal, 20)
            .frame(height: 38)
            .background(DeskLinkPalette.subtle)
        }
        .frame(minWidth: 760, minHeight: 520)
        .onDisappear { bridge.releaseAll() }
    }

    private var videoSize: CGSize? {
        guard let pixelBuffer = bridge.latestPixelBuffer else { return nil }
        return CGSize(width: CVPixelBufferGetWidth(pixelBuffer), height: CVPixelBufferGetHeight(pixelBuffer))
    }

    private var statusText: String {
        deskLinkSessionStatusText(for: bridge.state)
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
