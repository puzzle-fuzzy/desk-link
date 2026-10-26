import Foundation

/// Values are the peer's authenticated display inventory, without invented
/// screen names or assumptions about which screen was selected locally.
public struct RemoteDisplay: Decodable, Equatable, Identifiable, Sendable {
    public let id: UInt32
    public let width: UInt32
    public let height: UInt32
    public let isPrimary: Bool

    enum CodingKeys: String, CodingKey {
        case id, width, height
        case isPrimary = "is_primary"
    }

    public init(id: UInt32, width: UInt32, height: UInt32, isPrimary: Bool) {
        self.id = id; self.width = width; self.height = height; self.isPrimary = isPrimary
    }

    public var label: String { "屏幕 \(id) · \(width) × \(height)\(isPrimary ? " · 主屏幕" : "")" }
}

public enum VideoQualityPreference: String, CaseIterable, Codable, Identifiable, Sendable {
    case automatic, smooth, balanced, sharp
    public var id: Self { self }
    public var title: String {
        switch self { case .automatic: "自动"; case .smooth: "流畅"; case .balanced: "均衡"; case .sharp: "清晰" }
    }
    var cValue: UInt32 {
        switch self { case .automatic: 0; case .smooth: 1; case .balanced: 2; case .sharp: 3 }
    }
}

public enum VideoQualityPreset: String, Codable, Sendable {
    case smooth, balanced, sharp
    public var title: String {
        switch self { case .smooth: "流畅"; case .balanced: "均衡"; case .sharp: "清晰" }
    }
}

struct SessionFeatureEvent: Decodable {
    let kind: String
    let displays: [RemoteDisplay]?
    let selectedDisplayID: UInt32?
    let qualityPreference: VideoQualityPreference?
    let appliedPreset: VideoQualityPreset?
    let operation: String?

    enum CodingKeys: String, CodingKey {
        case kind, displays, operation
        case selectedDisplayID = "selected_display_id"
        case qualityPreference = "quality_preference"
        case appliedPreset = "applied_preset"
    }
}

struct ClipboardFeatureEvent: Decodable {
    let requestID: UInt64
    let status: String
    let operation: String
    let text: String?
    let result: String

    enum CodingKeys: String, CodingKey {
        case status, operation, text, result
        case requestID = "request_id"
    }
}

#if DEBUG
/// Intercepts commands before the FFI boundary; tests never access a live peer
/// or the system pasteboard.
public enum ControllerFeatureRequest: Equatable, Sendable {
    case selectDisplay(streamID: UInt64, displayID: UInt32)
    case chooseQuality(streamID: UInt64, preference: VideoQualityPreference)
    case sendClipboard(streamID: UInt64, requestID: UInt64, text: String)
    case requestClipboard(streamID: UInt64, requestID: UInt64)
    case sendText(streamID: UInt64, text: String)
}
#endif
