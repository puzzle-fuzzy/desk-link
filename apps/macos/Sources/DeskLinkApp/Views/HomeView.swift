import SwiftUI
import DeskLinkAppleCore

struct HomeView: View {
    @ObservedObject var bridge: ControllerBridge
    @StateObject private var draft = DeskLinkConnectionDraft()

    var body: some View {
        ControllerHomeView(bridge: bridge, draft: draft)
    }
}
