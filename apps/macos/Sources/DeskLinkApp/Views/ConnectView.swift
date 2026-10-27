import DeskLinkAppleCore
import DeskLinkC
import SwiftUI

struct DeskLinkConnectionStatus: Equatable {
    let title: String
    let systemImage: String
}

func deskLinkConnectionIsWaiting(_ state: ConnectionState) -> Bool {
    switch state {
    case .pairing, .connecting, .reconnecting, .recovering: true
    default: false
    }
}

func deskLinkConnectionWaitingDetail(_ state: ConnectionState) -> String? {
    switch state {
    case .connecting: "正在查找设备并建立安全连接。可以取消后更换设备。"
    case .pairing: "请在另一台设备上批准此次连接。可以随时取消。"
    case .reconnecting: "网络暂时中断，正在重新连接。可以取消后更换设备。"
    case .recovering: "正在等待远程画面恢复。可以随时取消。"
    default: nil
    }
}

func deskLinkConnectionStatus(for state: ConnectionState) -> DeskLinkConnectionStatus {
    switch state {
    case .idle, .closed:
        DeskLinkConnectionStatus(title: "准备连接", systemImage: "circle")
    case .pairing:
        DeskLinkConnectionStatus(title: "等待确认", systemImage: "person.crop.circle.badge.questionmark")
    case .connecting:
        DeskLinkConnectionStatus(title: "连接中", systemImage: "arrow.triangle.2.circlepath")
    case .connected:
        DeskLinkConnectionStatus(title: "已连接", systemImage: "checkmark.circle")
    case .reconnecting, .recovering:
        DeskLinkConnectionStatus(title: "正在恢复连接", systemImage: "arrow.triangle.2.circlepath")
    case .frozen:
        DeskLinkConnectionStatus(title: "画面暂时冻结", systemImage: "pause.circle")
    case .failed:
        DeskLinkConnectionStatus(title: "连接失败", systemImage: "exclamationmark.circle")
    }
}

/// UI-only draft survives main navigation without writing access passwords to
/// preferences or Keychain. The bridge remains the source of runtime state.
@MainActor
final class DeskLinkConnectionDraft: ObservableObject {
    @Published var deviceID = ""
    @Published var accessPassword = ""
    @Published private(set) var startedAt: Date?
    @Published private(set) var endedAt: Date?
    @Published private(set) var wasCancelled = false
    private var savedHost: SavedHost?
    private var usedDeviceCredentials = false
    private var attemptedDeviceID = ""
    private var attemptedPassword = ""

    var canRetry: Bool { savedHost != nil || usedDeviceCredentials }
    var targetName: String? { savedHost.map(deskLinkSavedHostTitle) }

    func connectDevice(using bridge: ControllerBridge) {
        savedHost = nil
        usedDeviceCredentials = true
        attemptedDeviceID = deviceID
        attemptedPassword = accessPassword
        begin()
        bridge.connect(deviceID: attemptedDeviceID, temporaryPassword: attemptedPassword)
        observe(bridge.state)
    }

    func connect(_ host: SavedHost, using bridge: ControllerBridge) {
        savedHost = host
        usedDeviceCredentials = false
        attemptedDeviceID = ""
        attemptedPassword = ""
        begin()
        bridge.connect(savedHost: host)
        observe(bridge.state)
    }

    func retry(using bridge: ControllerBridge) {
        guard canRetry else { return }
        begin()
        if let savedHost {
            bridge.connect(savedHost: savedHost)
        } else {
            bridge.connect(deviceID: attemptedDeviceID, temporaryPassword: attemptedPassword)
        }
        observe(bridge.state)
    }

    func cancel(using bridge: ControllerBridge) {
        wasCancelled = true
        endedAt = Date()
        bridge.disconnect()
    }

    func observe(_ state: ConnectionState) {
        switch state {
        case .failed, .closed, .connected:
            if startedAt != nil, endedAt == nil { endedAt = Date() }
        default: break
        }
    }

    private func begin() {
        startedAt = Date()
        endedAt = nil
        wasCancelled = false
    }
}

let deskLinkConnectionSteps = ["准备连接", "建立加密连接", "等待主机确认", "打开远程桌面"]

struct DeskLinkConnectionProgress: Equatable {
    let activeStep: Int?
    let guidance: String
}

func deskLinkConnectionProgress(for stage: ControllerConnectionStage, elapsed: TimeInterval) -> DeskLinkConnectionProgress {
    let delayed = elapsed >= 15
    switch stage {
    case .idle, .closed, .disconnecting:
        return .init(activeStep: nil, guidance: "当前没有正在进行的连接。")
    case .creatingSession, .connectingRelay:
        return .init(activeStep: 0, guidance: delayed
            ? "准备时间较长，请确认两台设备在线，并核对设备 ID 和密码。"
            : "正在联系中继服务并查找远程设备。")
    case .secureHandshake:
        return .init(activeStep: 1, guidance: delayed
            ? "安全连接仍未完成，请保持两台设备联网，并确认主机没有退出 DeskLink。"
            : "正在校验远程设备身份并建立加密通道。")
    case .waitingApproval:
        return .init(activeStep: 2, guidance: delayed
            ? "请查看主机屏幕上的控制请求。如果没有出现请求，可取消后重试。"
            : "请在另一台设备上确认允许本次远程控制。已批准设备可能自动通过。")
    case .negotiating, .startingVideo:
        return .init(activeStep: 3, guidance: delayed
            ? "正在等待远程画面，请确认主机屏幕可用。也可以取消后重新连接。"
            : "正在准备远程画面，桌面就绪后会自动打开。")
    case .connected:
        return .init(activeStep: 3, guidance: "远程桌面已连接。")
    case .degraded, .recoveringVideo, .reconnecting:
        return .init(activeStep: 1, guidance: "网络连接发生变化，DeskLink 正在自动恢复会话。")
    }
}

func deskLinkConnectionElapsed(_ interval: TimeInterval) -> String {
    let seconds = max(0, Int(interval))
    if seconds == 0 { return "刚刚开始" }
    if seconds < 60 { return "已等待 \(seconds) 秒" }
    if seconds % 60 == 0 { return "已等待 \(seconds / 60) 分钟" }
    return "已等待 \(seconds / 60) 分 \(seconds % 60) 秒"
}

/// First-use guidance shown only while no recent device exists: the three
/// steps from a fresh install to a working session.
struct DeskLinkFirstUseGuide: View {
    var body: some View {
        DeskLinkPanel(background: DeskLinkPalette.infoSurface) {
            VStack(alignment: .leading, spacing: 12) {
                Label("开始使用 DeskLink", systemImage: "lightbulb")
                    .font(.system(size: 16, weight: .semibold))
                    .foregroundStyle(DeskLinkPalette.ink)
                VStack(alignment: .leading, spacing: 8) {
                    DeskLinkFirstUseGuideStep(number: 1, title: "在主机上开启共享",
                        detail: "在要被控制的电脑上打开 DeskLink，进入“共享此设备”，点“开始共享”。")
                    DeskLinkFirstUseGuideStep(number: 2, title: "记下设备 ID 和密码",
                        detail: "主机页面会显示 12 位设备 ID，以及临时密码或你设置的固定密码。")
                    DeskLinkFirstUseGuideStep(number: 3, title: "在此输入并连接",
                        detail: "在下方输入设备 ID 和密码，点“连接设备”；主机批准后即可远控。")
                }
            }
        }
        .accessibilityIdentifier("first-use-guide")
    }
}

struct DeskLinkFirstUseGuideStep: View {
    let number: Int
    let title: String
    let detail: String

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Text("\(number)")
                .font(.system(size: 13, weight: .bold))
                .foregroundStyle(.white)
                .frame(width: 22, height: 22)
                .background(Circle().fill(DeskLinkPalette.primary))
                .accessibilityLabel("第 \(number) 步")
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.system(size: 13, weight: .medium))
                Text(detail)
                    .font(.system(size: 12))
                    .foregroundStyle(DeskLinkPalette.secondaryInk)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }
}

struct ConnectView: View {
    @ObservedObject var bridge: ControllerBridge
    @ObservedObject var draft: DeskLinkConnectionDraft
    /// Hides the first-use guide as soon as one saved host exists.
    var hasSavedHosts = false

    /// The guide is a first-run affordance: one saved host permanently
    /// replaces it with the regular connect panel flow.
    var firstUseGuideVisible: Bool { !hasSavedHosts }

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            if firstUseGuideVisible {
                DeskLinkFirstUseGuide()
            }
            DeskLinkPanel(background: DeskLinkPalette.infoSurface) {
                VStack(alignment: .leading, spacing: 16) {
                    Label("使用设备 ID 和密码连接", systemImage: "desktopcomputer.and.arrow.right")
                        .font(.system(size: 16, weight: .semibold))
                        .foregroundStyle(DeskLinkPalette.ink)
                    Text("输入另一台设备显示的设备 ID 和访问密码。连接成功并获得批准后，可从最近设备再次连接。")
                        .font(.system(size: 12))
                        .foregroundStyle(DeskLinkPalette.secondaryInk)
                        .fixedSize(horizontal: false, vertical: true)

                    VStack(alignment: .leading, spacing: 6) {
                        Text("设备 ID").font(.system(size: 12, weight: .medium))
                        TextField("例如 123 456 789 012", text: $draft.deviceID)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 14, design: .monospaced))
                            .accessibilityLabel("设备 ID")
                    }
                    VStack(alignment: .leading, spacing: 6) {
                        Text("访问密码").font(.system(size: 12, weight: .medium))
                        SecureField("临时密码或固定密码", text: $draft.accessPassword)
                            .textFieldStyle(.roundedBorder)
                            .font(.system(size: 14, design: .monospaced))
                            .accessibilityLabel("访问密码")
                    }
                    .disabled(isBusy)

                    Button {
                        draft.connectDevice(using: bridge)
                    } label: {
                        Text("连接设备").frame(maxWidth: .infinity)
                    }
                    .buttonStyle(DeskLinkPrimaryButtonStyle())
                    .keyboardShortcut(.defaultAction)
                    .disabled(isBusy || !directoryCredentialsAreComplete)
                }
                .disabled(isBusy)
            }

            if draft.startedAt != nil || isBusy || hasFailed {
                TimelineView(.periodic(from: Date(), by: 1)) { context in
                    progressPanel(now: context.date)
                }
            }
        }
        .onChange(of: bridge.state) { state in draft.observe(state) }
    }

    private func progressPanel(now: Date) -> some View {
        let startedAt = bridge.connectionAttemptStartedAt ?? draft.startedAt ?? now
        let elapsed = max(0, (draft.endedAt ?? now).timeIntervalSince(startedAt))
        let progress = deskLinkConnectionProgress(for: bridge.connectionStage, elapsed: elapsed)
        let terminal = hasFailed || draft.wasCancelled || bridge.state == .closed
        return DeskLinkPanel {
            VStack(alignment: .leading, spacing: 14) {
                HStack {
                    if isBusy { ProgressView().controlSize(.small) }
                    Label(progressTitle, systemImage: deskLinkConnectionStatus(for: bridge.state).systemImage)
                        .font(.system(size: 13, weight: .semibold))
                    Spacer(minLength: 8)
                    Text(deskLinkConnectionElapsed(elapsed))
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(DeskLinkPalette.secondaryInk)
                }
                if let target = draft.targetName {
                    Text(target)
                        .font(.system(size: 12))
                        .foregroundStyle(DeskLinkPalette.secondaryInk)
                        .lineLimit(1)
                }
                VStack(alignment: .leading, spacing: 10) {
                    ForEach(deskLinkConnectionSteps.indices, id: \.self) { index in
                        let isActive = progress.activeStep == index
                        let complete = progress.activeStep.map { index < $0 } ?? false
                        HStack(spacing: 9) {
                            Image(systemName: complete ? "checkmark.circle.fill" : isActive ? "circle.inset.filled" : "circle")
                                .foregroundStyle(complete ? DeskLinkPalette.success : isActive ? DeskLinkPalette.primary : DeskLinkPalette.mutedInk)
                            Text(deskLinkConnectionSteps[index])
                                .font(.system(size: 12, weight: isActive ? .semibold : .regular))
                                .foregroundStyle(isActive ? DeskLinkPalette.ink : DeskLinkPalette.secondaryInk)
                        }
                    }
                }
                .accessibilityElement(children: .combine)
                .accessibilityLabel("远程连接进度")
                Text(draft.wasCancelled ? "已取消本次连接。可以修改设备信息或重试。" : hasFailed ? "请检查设备 ID、密码和主机状态，然后重试。" : progress.guidance)
                    .font(.system(size: 12))
                    .foregroundStyle(elapsed >= 15 && !terminal ? DeskLinkPalette.warning : DeskLinkPalette.secondaryInk)
                    .fixedSize(horizontal: false, vertical: true)
                if isBusy {
                    Button("取消连接") { draft.cancel(using: bridge) }
                        .buttonStyle(DeskLinkSecondaryButtonStyle())
                } else if terminal && draft.canRetry {
                    Button("重试本次连接") { draft.retry(using: bridge) }
                        .buttonStyle(DeskLinkPrimaryButtonStyle())
                }
            }
        }
    }

    private var progressTitle: String {
        if draft.wasCancelled { return "已取消连接" }
        if bridge.state == .closed { return "已断开连接" }
        return deskLinkConnectionStatus(for: bridge.state).title
    }

    private var hasFailed: Bool {
        if case .failed = bridge.state { return true }
        return false
    }

    private var isBusy: Bool {
        deskLinkConnectionIsWaiting(bridge.state)
    }

    private var directoryCredentialsAreComplete: Bool {
        draft.deviceID.filter(\.isNumber).count == 12
            && (8...64).contains(
                draft.accessPassword.trimmingCharacters(in: .whitespacesAndNewlines).utf8.count)
    }
}
