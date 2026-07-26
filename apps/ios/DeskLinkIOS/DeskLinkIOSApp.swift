import DeskLinkAppleCore
import SwiftUI
import UIKit

@main
struct DeskLinkIOSApp: App {
    @StateObject private var controller: ControllerBridge
    @StateObject private var account: AccountClient

    init() {
        _controller = StateObject(
            wrappedValue: ControllerBridge(configuration: IOSRuntimeConfiguration.production)
        )
        _account = StateObject(
            wrappedValue: Self.makeAccountClient()
        )
    }

    private static func makeAccountClient() -> AccountClient {
        let client = AccountClient(
            baseURL: IOSRuntimeConfiguration.accountURL,
            platform: .ios,
            deviceName: UIDevice.current.name
        )
#if DEBUG
        if ProcessInfo.processInfo.arguments.contains("-DeskLinkUITestSignedIn") {
            client.activateForTesting()
        }
#endif
        return client
    }

    var body: some Scene {
        WindowGroup {
            IOSAuthenticatedRootView(account: account, controller: controller)
                .deskLinkSessionLifecycle(controller: controller)
        }
    }
}

struct IOSAuthenticatedRootView: View {
    @ObservedObject var account: AccountClient
    @ObservedObject var controller: ControllerBridge

    var body: some View {
        Group {
            switch account.state {
            case .loading:
                ProgressView("正在准备 DeskLink…")
            case .signedOut:
                AccountLoginView(account: account)
            case .signedIn:
                IOSRootView(account: account, controller: controller)
            }
        }
        .task { await account.restore() }
    }
}
