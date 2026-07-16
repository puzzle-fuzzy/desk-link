import SwiftUI

struct RolePickerView: View {
    let selectRole: (AppRole) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack(spacing: 12) {
                DeskLinkMark()
                Text("DeskLink")
                    .font(.system(size: 24, weight: .semibold))
                    .foregroundStyle(DeskLinkPalette.ink)
            }
            Text("选择此 Mac 在远程会话中的用途。")
                .font(.system(size: 14))
                .foregroundStyle(DeskLinkPalette.secondaryInk)
            HStack(spacing: 10) {
                Button("控制另一台设备") { selectRole(.controller) }
                    .buttonStyle(DeskLinkPrimaryButtonStyle())
                Button("共享此 Mac") { selectRole(.host) }
                    .buttonStyle(DeskLinkSecondaryButtonStyle())
            }
        }
        .padding(28)
        .frame(minWidth: 500, minHeight: 230)
        .background(DeskLinkPalette.surface)
    }
}
