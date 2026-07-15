import ApplicationServices
import CoreGraphics
import Foundation

enum MacPermissionStatus: Equatable, Sendable {
    case granted
    case denied
}

struct MacPermissionSnapshot: Equatable, Sendable {
    let screenRecording: MacPermissionStatus
    let accessibility: MacPermissionStatus
    let screenRecordingSettingsURL: URL
    let accessibilitySettingsURL: URL

    var canCaptureAndControl: Bool {
        screenRecording == .granted && accessibility == .granted
    }

    static let denied = MacPermissionSnapshot(
        screenRecording: .denied,
        accessibility: .denied,
        screenRecordingSettingsURL: MacPermissions.screenRecordingSettingsURL,
        accessibilitySettingsURL: MacPermissions.accessibilitySettingsURL
    )
}

protocol MacPermissionProvider {
    var screenRecordingGranted: Bool { get }
    var accessibilityGranted: Bool { get }
    func requestScreenRecording() -> Bool
    func requestAccessibility() -> Bool
}

struct StaticMacPermissionProvider: MacPermissionProvider {
    let screenRecordingGranted: Bool
    let accessibilityGranted: Bool

    func requestScreenRecording() -> Bool { screenRecordingGranted }
    func requestAccessibility() -> Bool { accessibilityGranted }
}

struct SystemMacPermissionProvider: MacPermissionProvider {
    var screenRecordingGranted: Bool { CGPreflightScreenCaptureAccess() }
    var accessibilityGranted: Bool { AXIsProcessTrusted() }

    func requestScreenRecording() -> Bool { CGRequestScreenCaptureAccess() }

    func requestAccessibility() -> Bool {
        AXIsProcessTrustedWithOptions(["AXTrustedCheckOptionPrompt": true] as CFDictionary)
    }
}

struct MacPermissions {
    static let screenRecordingSettingsURL = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")!
    static let accessibilitySettingsURL = URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")!

    private let provider: any MacPermissionProvider

    init(provider: any MacPermissionProvider = SystemMacPermissionProvider()) {
        self.provider = provider
    }

    func snapshot() -> MacPermissionSnapshot {
        MacPermissionSnapshot(
            screenRecording: provider.screenRecordingGranted ? .granted : .denied,
            accessibility: provider.accessibilityGranted ? .granted : .denied,
            screenRecordingSettingsURL: Self.screenRecordingSettingsURL,
            accessibilitySettingsURL: Self.accessibilitySettingsURL
        )
    }

    func requestScreenRecording() -> MacPermissionSnapshot {
        _ = provider.requestScreenRecording()
        return snapshot()
    }

    func requestAccessibility() -> MacPermissionSnapshot {
        _ = provider.requestAccessibility()
        return snapshot()
    }
}
