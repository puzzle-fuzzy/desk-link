import SwiftUI

struct RolePickerView: View {
    let selectRole: (AppRole) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("DeskLink")
                .font(.largeTitle.bold())
            Text("Choose how this Mac participates in a remote session.")
                .foregroundStyle(.secondary)
            HStack(spacing: 12) {
                Button("Control another Mac") { selectRole(.controller) }
                    .buttonStyle(.borderedProminent)
                Button("Share this Mac") { selectRole(.host) }
                    .buttonStyle(.bordered)
            }
        }
        .padding(28)
        .frame(minWidth: 500, minHeight: 230)
    }
}
