import SwiftUI

@main
struct DeskLinkApp: App {
    @StateObject private var bridge = RustBridge()

    var body: some Scene {
        WindowGroup {
            switch bridge.state {
            case .connected:
                SessionView(bridge: bridge)
            default:
                HomeView(bridge: bridge)
            }
        }
    }
}
