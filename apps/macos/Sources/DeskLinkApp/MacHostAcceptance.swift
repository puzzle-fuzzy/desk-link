#if DEBUG
import DeskLinkAppleCore
import DeskLinkC
import Foundation

/// This separate debug bundle never accesses a real identity, Keychain, relay,
/// screen capture or input injector. Production builds contain no fixture entry.
@MainActor
enum MacHostAcceptance {
    static var isEnabled: Bool {
        ProcessInfo.processInfo.environment["DESKLINK_HOST_ACCEPTANCE"] == "1"
    }

    static func host() -> HostBridge {
        let store = HostFixedAccessStore(keychain: InMemoryKeychainStore())
        try? store.save(HostAccessPassword("ABCD2345"))
        return HostBridge(
            permissions: MacPermissions(provider: StaticMacPermissionProvider(
                screenRecordingGranted: true, accessibilityGranted: true)),
            loadStoredTrust: false,
            identityLoader: { HostIdentity(deviceID: Array(0..<16), secretKey: [UInt8](repeating: 9, count: 32)) },
            fixedAccessStore: store,
            runtimeFactory: { _, _ in AcceptanceRuntime() })
    }

    private final class AcceptanceRuntime: HostDirectoryRuntime, @unchecked Sendable {
        var nativeHandle: OpaquePointer? { nil }
        func register(_ request: HostDirectoryStartRequest) -> HostDirectoryRegistration {
            Thread.sleep(forTimeInterval: 0.3)
            return HostDirectoryRegistration(resultCode: DESKLINK_OK.rawValue,
                expiresAt: request.mode == .temporary ? Date().addingTimeInterval(600) : Date(timeIntervalSince1970: 0),
                encodedInvite: Data(repeating: 7, count: Int(DESKLINK_PAIRING_INVITE_BYTES)))
        }
        func stopAndDestroy() {}
    }
}
#endif
