import DeskLinkAppleCore
import Foundation

enum IOSRuntimeConfiguration {
    private static let managedRelayURL = "quic://turn.p2p.yxswy.com:4433"
    private static let managedRelayServerName = "turn.p2p.yxswy.com"

    static var production: DeskLinkRuntimeConfiguration {
        DeskLinkRuntimeConfiguration(
            relayURL: configuredValue("DeskLinkRelayURL", fallback: managedRelayURL),
            relayServerName: configuredValue("DeskLinkRelayServerName", fallback: managedRelayServerName),
            platform: .ios
        )
    }

    static var accountURL: URL {
        let value = configuredValue(
            "DeskLinkAccountURL",
            fallback: "https://account.p2p.yxswy.com"
        )
        return URL(string: value) ?? URL(string: "https://account.p2p.yxswy.com")!
    }

    static var test: DeskLinkRuntimeConfiguration {
        DeskLinkRuntimeConfiguration(
            relayURL: "quic://127.0.0.1:4433",
            relayServerName: "localhost",
            platform: .ios
        )
    }

    private static func configuredValue(_ key: String, fallback: String) -> String {
        if let value = Bundle.main.object(forInfoDictionaryKey: key) as? String,
           !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        {
            return value
        }
        if let value = UserDefaults.standard.string(forKey: key),
           !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        {
            return value
        }
        return fallback
    }
}
