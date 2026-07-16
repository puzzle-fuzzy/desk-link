import SwiftUI

struct HomeView: View {
    @ObservedObject var bridge: ControllerBridge

    var body: some View {
        ControllerHomeView(bridge: bridge)
    }
}
