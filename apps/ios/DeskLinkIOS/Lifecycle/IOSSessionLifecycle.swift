import DeskLinkAppleCore
import SwiftUI

/// Bridges iOS scene transitions to the controller runtime. Foreground resume
/// is deliberately delayed slightly so a rapid background/active bounce does
/// not create duplicate native workers.
struct IOSSessionLifecycle: ViewModifier {
    @Environment(\.scenePhase) private var scenePhase
    @ObservedObject var controller: ControllerBridge
    @State private var resumeTask: Task<Void, Never>?

    func body(content: Content) -> some View {
        content
            .onChange(of: scenePhase) { phase in
                switch phase {
                case .background:
                    resumeTask?.cancel()
                    resumeTask = nil
                    controller.suspendForBackground()
                case .active:
                    resumeTask?.cancel()
                    resumeTask = Task { @MainActor in
                        try? await Task.sleep(nanoseconds: 250_000_000)
                        guard !Task.isCancelled else { return }
                        controller.resumeFromBackground()
                    }
                case .inactive:
                    break
                @unknown default:
                    break
                }
            }
            .onDisappear {
                resumeTask?.cancel()
            }
    }
}

extension View {
    func deskLinkSessionLifecycle(controller: ControllerBridge) -> some View {
        modifier(IOSSessionLifecycle(controller: controller))
    }
}
