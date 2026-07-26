import SwiftUI
import DeskLinkAppleCore

struct HomeView: View {
    @ObservedObject var bridge: ControllerBridge

    var body: some View {
        ControllerHomeView(bridge: bridge)
    }
}
