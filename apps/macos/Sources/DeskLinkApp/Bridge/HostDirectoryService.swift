import DeskLinkC
import Foundation

enum HostAccessMode: String, CaseIterable, Identifiable, Hashable, Sendable {
    case temporary
    case fixed

    var id: Self { self }
    var title: String { self == .temporary ? "临时密码" : "固定密码" }
}

enum HostSharingStatus: Equatable, Sendable {
    case off
    case registering
    case ready
    case connected
    case stopping
    case failed(String)
}

struct HostDirectoryStartRequest: Sendable {
    let deviceNumber: UInt64
    let password: HostAccessPassword
    let mode: HostAccessMode
}

struct HostDirectoryRegistration: Sendable {
    let resultCode: UInt32
    let expiresAt: Date
    let encodedInvite: Data
}

/// Registration may perform blocking network work. The bridge runs both methods
/// off the main actor, and waits for registration to finish before destruction.
protocol HostDirectoryRuntime: AnyObject, Sendable {
    var nativeHandle: OpaquePointer? { get }
    func register(_ request: HostDirectoryStartRequest) -> HostDirectoryRegistration
    func approve(controllerDeviceID: [UInt8], controllerVerifyKey: [UInt8]) -> UInt32
    func stopAndDestroy()
}

extension HostDirectoryRuntime {
    func approve(controllerDeviceID: [UInt8], controllerVerifyKey: [UInt8]) -> UInt32 {
        guard let nativeHandle,
              controllerDeviceID.count == TrustedController.deviceIDLength,
              controllerVerifyKey.count == TrustedController.verifyKeyLength else {
            return DESKLINK_INVALID_ARGUMENT.rawValue
        }
        return controllerDeviceID.withUnsafeBytes { deviceID in
            controllerVerifyKey.withUnsafeBytes { verifyKey in
                desklink_host_approve(nativeHandle, deviceID.bindMemory(to: UInt8.self).baseAddress,
                    verifyKey.bindMemory(to: UInt8.self).baseAddress).rawValue
            }
        }
    }
}

protocol HostControllerTrustStoring {
    func list() throws -> [TrustedController]
    func trust(_ controller: TrustedController) throws
    @discardableResult func revoke(deviceID: [UInt8]) throws -> Bool
}

extension TrustedControllerStore: HostControllerTrustStoring {}

typealias HostDirectoryRuntimeFactory = @Sendable (
    HostIdentity, @escaping @Sendable (HostCallbackEvent) -> Void
) throws -> any HostDirectoryRuntime
