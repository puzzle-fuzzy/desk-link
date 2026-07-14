import SwiftUI

@main
struct DeskLinkApp: App {
    @StateObject private var bridge = RustBridge()

    var body: some Scene {
        WindowGroup {
            HomeView(bridge: bridge)
        }
        .windowResizability(.contentSize)
    }
}
