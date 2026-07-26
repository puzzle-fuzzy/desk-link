import DeskLinkAppleCore
import SwiftUI

@main
struct DeskLinkIOSApp: App {
    @StateObject private var controller: ControllerBridge

    init() {
        _controller = StateObject(
            wrappedValue: ControllerBridge(configuration: IOSRuntimeConfiguration.production)
        )
    }

    var body: some Scene {
        WindowGroup {
            IOSRootView(controller: controller)
                .deskLinkSessionLifecycle(controller: controller)
        }
    }
}
