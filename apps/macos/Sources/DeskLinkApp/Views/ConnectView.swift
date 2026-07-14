import SwiftUI

struct ConnectView: View {
    @ObservedObject var bridge: RustBridge
    @State private var code = ""

    var body: some View {
        Form {
            TextField("8-character code", text: $code)
                .textCase(.uppercase)
            Button("Connect") {
                bridge.connect(code: code.trimmingCharacters(in: .whitespacesAndNewlines))
            }
            .disabled(code.count != 8)
        }
        .padding()
    }
}
