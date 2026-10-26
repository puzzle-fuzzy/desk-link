import AppKit
import SwiftUI

struct HostAccessPanel: View {
    @ObservedObject var bridge: HostBridge
    @State private var passwordVisible = false
    @State private var showingFixedPassword = false
    @State private var showingRemovePassword = false
    @State private var pendingAction: AccessChange?

    private enum AccessChange: Identifiable {
        case mode(HostAccessMode)
        case refresh
        var id: String { String(describing: self) }
    }

    private var hasSession: Bool { bridge.state == .connected || bridge.pendingApproval != nil }
    private var sharingActive: Bool {
        switch bridge.sharingStatus {
        case .registering, .ready, .connected, .stopping: true
        case .off, .failed: false
        }
    }

    var body: some View {
        DeskLinkPanel(background: DeskLinkPalette.infoSurface) {
            VStack(alignment: .leading, spacing: 18) {
                HStack(alignment: .center, spacing: 16) {
                    VStack(alignment: .leading, spacing: 7) {
                        Text("本机设备 ID")
                            .font(.system(size: 12))
                            .foregroundStyle(DeskLinkPalette.secondaryInk)
                        Text(bridge.publicDeviceID ?? "正在读取本机身份…")
                            .font(.system(size: bridge.publicDeviceID == nil ? 15 : 28, weight: .semibold, design: .monospaced))
                            .textSelection(.enabled)
                    }
                    Spacer()
                    Button {
                        if let id = bridge.publicDeviceID { copy(id) }
                    } label: { Label("复制设备 ID", systemImage: "doc.on.doc") }
                    .buttonStyle(DeskLinkSecondaryButtonStyle())
                    .disabled(bridge.publicDeviceID == nil)
                }

                Divider()

                Picker("访问方式", selection: Binding(
                    get: { bridge.accessMode },
                    set: { mode in requestChange(.mode(mode)) }
                )) {
                    ForEach(HostAccessMode.allCases) { mode in Text(mode.title).tag(mode) }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .frame(maxWidth: 320)
                .disabled(bridge.isUpdatingCredentials || bridge.sharingStatus == .stopping)

                if bridge.accessMode == .temporary {
                    TimelineView(.periodic(from: Date(), by: 1)) { context in
                        temporaryPassword(now: context.date)
                    }
                } else {
                    fixedPassword
                }

                HStack(spacing: 12) {
                    if sharingActive {
                        Button(bridge.sharingStatus == .stopping ? "正在停止…" : "停止共享") { bridge.stop() }
                            .buttonStyle(DeskLinkSecondaryButtonStyle())
                            .disabled(bridge.sharingStatus == .stopping)
                    } else {
                        Button("开启共享") { bridge.startSharing() }
                            .buttonStyle(DeskLinkPrimaryButtonStyle())
                            .disabled(!bridge.permissions.canCaptureAndControl || bridge.publicDeviceID == nil
                                || bridge.isUpdatingCredentials || (bridge.accessMode == .fixed && !bridge.hasFixedPassword))
                    }
                    Text(bridge.permissions.canCaptureAndControl
                        ? "新设备首次连接需要在此 Mac 上批准。"
                        : "请先在下方允许屏幕录制和辅助功能。")
                        .font(.system(size: 12))
                        .foregroundStyle(DeskLinkPalette.secondaryInk)
                        .fixedSize(horizontal: false, vertical: true)
                }

                if let error = bridge.credentialsError {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(error).font(.system(size: 12)).foregroundStyle(DeskLinkPalette.error)
                        Button("重新读取") { Task { await bridge.prepareAccessCredentials() } }
                    }
                }
            }
        }
        .onChange(of: bridge.temporaryPassword) { _ in passwordVisible = false }
        .onChange(of: bridge.accessMode) { _ in passwordVisible = false }
        .sheet(isPresented: $showingFixedPassword) {
            HostFixedPasswordEditor(bridge: bridge)
        }
        .alert("移除固定密码？", isPresented: $showingRemovePassword) {
            Button("取消", role: .cancel) {}
            Button("移除", role: .destructive) { Task { await bridge.removeFixedPassword() } }
        } message: {
            Text("固定密码将从钥匙串移除。当前使用固定密码的共享会停止，并切换为临时密码。")
        }
        .alert(item: $pendingAction) { action in
            Alert(title: Text("更新访问方式？"),
                  message: Text("当前连接将断开，原临时密码和二维码会失效。对方需要使用新的访问方式重新连接。"),
                  primaryButton: .default(Text("继续")) { apply(action) },
                  secondaryButton: .cancel(Text("取消")))
        }
    }

    private func temporaryPassword(now: Date) -> some View {
        let expiresAt = bridge.temporaryPasswordExpiresAt
        let password = expiresAt.map { $0 > now ? bridge.temporaryPassword : nil } ?? nil
        return VStack(alignment: .leading, spacing: 9) {
            HStack(spacing: 12) {
                Text(password == nil ? (bridge.sharingStatus == .registering ? "正在生成…" : "开启共享后生成")
                     : (passwordVisible ? password! : "••••••••"))
                    .font(.system(size: password == nil ? 15 : 24, weight: .medium, design: .monospaced))
                    .privacySensitive()
                    .accessibilityLabel(password == nil ? "临时密码未就绪" : (passwordVisible ? "临时密码 \(password!)" : "临时密码已隐藏"))
                if password != nil {
                    Button(passwordVisible ? "隐藏" : "显示") { passwordVisible.toggle() }
                        .buttonStyle(DeskLinkSecondaryButtonStyle())
                    Button("复制密码") { if let password { copy(password) } }
                        .buttonStyle(DeskLinkSecondaryButtonStyle())
                }
                Spacer(minLength: 0)
                if sharingActive {
                    Button { requestChange(.refresh) } label: { Label("刷新密码", systemImage: "arrow.clockwise") }
                        .buttonStyle(DeskLinkSecondaryButtonStyle())
                        .disabled(bridge.sharingStatus == .registering || bridge.sharingStatus == .stopping)
                }
            }
            if let expiresAt, password != nil {
                Text("\(max(1, Int(ceil(expiresAt.timeIntervalSince(now) / 60)))) 分钟内有效 · 刷新后原密码和二维码失效")
                    .font(.system(size: 12)).foregroundStyle(DeskLinkPalette.secondaryInk)
            } else {
                Text("临时密码有效期为 10 分钟，关闭共享后立即失效。")
                    .font(.system(size: 12)).foregroundStyle(DeskLinkPalette.secondaryInk)
            }
        }
    }

    private var fixedPassword: some View {
        HStack(spacing: 16) {
            VStack(alignment: .leading, spacing: 7) {
                Label(bridge.credentialsError != nil && !bridge.hasFixedPassword ? "固定密码状态未就绪" : (bridge.hasFixedPassword ? "已设置固定密码" : "尚未设置固定密码"),
                      systemImage: bridge.hasFixedPassword ? "lock.shield" : "lock")
                    .font(.system(size: 15, weight: .medium))
                Text("固定密码保存在本机钥匙串中，开启共享时生效。")
                    .font(.system(size: 12)).foregroundStyle(DeskLinkPalette.secondaryInk)
            }
            Spacer(minLength: 0)
            Button(bridge.hasFixedPassword ? "更改密码" : "设置密码") { showingFixedPassword = true }
                .buttonStyle(DeskLinkSecondaryButtonStyle())
                .disabled(bridge.isUpdatingCredentials)
            if bridge.hasFixedPassword {
                Button("移除") { showingRemovePassword = true }
                    .buttonStyle(DeskLinkSecondaryButtonStyle())
                    .disabled(bridge.isUpdatingCredentials)
            }
        }
    }

    private func requestChange(_ action: AccessChange) {
        if hasSession { pendingAction = action } else { apply(action) }
    }

    private func apply(_ action: AccessChange) {
        switch action {
        case let .mode(mode): bridge.setAccessMode(mode)
        case .refresh: bridge.refreshTemporaryPassword()
        }
    }

    private func copy(_ text: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }
}

enum HostFixedPasswordValidation {
    static func message(password: String, confirmation: String) -> String? {
        guard let password = try? HostAccessPassword(password) else {
            return HostAccessCredentialsError.invalidPassword.errorDescription
        }
        guard let confirmation = try? HostAccessPassword(confirmation), password == confirmation else {
            return "两次输入的密码不一致。"
        }
        return nil
    }
}

private struct HostFixedPasswordEditor: View {
    @ObservedObject var bridge: HostBridge
    @Environment(\.dismiss) private var dismiss
    @State private var password = ""
    @State private var confirmation = ""
    @State private var validationError: String?
    @State private var saving = false

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text(bridge.hasFixedPassword ? "更改固定密码" : "设置固定密码")
                .font(.system(size: 20, weight: .semibold))
            Text("请输入 8 位字母或数字，不支持 0、1、I、O；字母不区分大小写。")
                .font(.system(size: 13)).foregroundStyle(DeskLinkPalette.secondaryInk)
                .fixedSize(horizontal: false, vertical: true)
            SecureField("新密码", text: $password)
                .textFieldStyle(.roundedBorder)
                .accessibilityLabel("新固定密码")
            SecureField("再次输入", text: $confirmation)
                .textFieldStyle(.roundedBorder)
                .accessibilityLabel("确认固定密码")
            if bridge.accessMode == .fixed && bridge.sharingStatus != .off {
                Text("保存后将重启共享，当前连接会断开。")
                    .font(.system(size: 12)).foregroundStyle(DeskLinkPalette.warning)
            }
            if let message = validationError {
                Text(message).font(.system(size: 12)).foregroundStyle(DeskLinkPalette.error)
            }
            HStack {
                Spacer()
                Button("取消", role: .cancel) { dismiss() }.disabled(saving)
                Button(saving ? "正在保存…" : "保存") { save() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(saving || password.isEmpty || confirmation.isEmpty)
            }
        }
        .padding(28)
        .frame(width: 420)
        .interactiveDismissDisabled(saving)
        .onDisappear { password = ""; confirmation = "" }
    }

    private func save() {
        validationError = HostFixedPasswordValidation.message(password: password, confirmation: confirmation)
        guard validationError == nil else { return }
        saving = true
        Task { @MainActor in
            let saved = await bridge.setFixedPassword(password)
            saving = false
            if saved { password = ""; confirmation = ""; dismiss() }
            else { validationError = bridge.credentialsError ?? "固定密码未保存，请重试。" }
        }
    }
}
