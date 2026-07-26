import DeskLinkAppleCore
import Foundation

enum IOSRuntimeConfiguration {
    static var production: DeskLinkRuntimeConfiguration {
        DeskLinkRuntimeConfiguration(
            relayURL: configuredValue("DeskLinkRelayURL"),
            relayServerName: configuredValue("DeskLinkRelayServerName"),
            platform: .ios
        )
    }

    static var test: DeskLinkRuntimeConfiguration {
        DeskLinkRuntimeConfiguration(
            relayURL: "quic://127.0.0.1:4433",
            relayServerName: "localhost",
            platform: .ios
        )
    }

    private static func configuredValue(_ key: String) -> String {
        if let value = Bundle.main.object(forInfoDictionaryKey: key) as? String {
            return value
        }
        return UserDefaults.standard.string(forKey: key) ?? ""
    }
}
