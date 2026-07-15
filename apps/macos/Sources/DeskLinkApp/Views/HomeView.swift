import SwiftUI

struct HomeView: View {
    @ObservedObject var bridge: ControllerBridge
    let chooseRole: () -> Void

    var body: some View {
        ControllerHomeView(bridge: bridge, chooseRole: chooseRole)
    }
}
