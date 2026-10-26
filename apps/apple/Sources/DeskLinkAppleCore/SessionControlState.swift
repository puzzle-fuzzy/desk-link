import Foundation

public struct SessionDisplay: Identifiable, Equatable, Sendable {
    public let id: UInt32
    public let width: UInt16
    public let height: UInt16
    public let primary: Bool
}

public enum SessionVideoQualityPreference: UInt32, CaseIterable, Identifiable, Sendable {
    case automatic = 0, smooth = 1, balanced = 2, sharp = 3
    public var id: UInt32 { rawValue }
    public var title: String {
        switch self {
        case .automatic: "自动"
        case .smooth: "流畅"
        case .balanced: "均衡"
        case .sharp: "清晰"
        }
    }
}

public enum SessionVideoQualityPreset: UInt32, Sendable {
    case smooth = 0, balanced = 1, sharp = 2
    public var title: String {
        switch self {
        case .smooth: "流畅"
        case .balanced: "均衡"
        case .sharp: "清晰"
        }
    }
}

public struct SessionVideoQuality: Equatable, Sendable {
    public let preference: SessionVideoQualityPreference
    public let preset: SessionVideoQualityPreset
}

public enum SessionInputRestrictionReason: UInt32, Sendable {
    case blocked = 0, unsupportedKey = 1, invalidInput = 2
    public var message: String {
        switch self {
        case .blocked: "远端当前窗口暂不能接收输入，请切回普通桌面或在远端完成授权。"
        case .unsupportedKey: "远端不支持此按键，可改用其他按键或文本输入。"
        case .invalidInput: "输入暂未送达，请在远端画面恢复后重试。"
        }
    }
}

/// Selected ControlMessage postcard variants, checked against Rust's real codec.
/// Unknown messages remain available through the bridge's raw control event.
public enum SessionControlState: Equatable, Sendable {
    case displays([SessionDisplay], activeID: UInt32)
    case audio(available: Bool, enabled: Bool)
    case videoQuality(SessionVideoQuality)
    case inputRestriction(SessionInputRestrictionReason?)

    public init?(encoded: Data) {
        // Sixteen maximal display entries fit below this bound. Validate before
        // allocating the temporary byte buffer or the display array.
        guard !encoded.isEmpty, encoded.count <= 256 else { return nil }
        var reader = SessionControlReader(bytes: Array(encoded))
        guard let variant = reader.unsigned(bits: 32) else { return nil }
        switch variant {
        case 4:
            guard let count = reader.unsigned(bits: 32), (1...16).contains(count) else { return nil }
            var displays: [SessionDisplay] = []
            var identifiers = Set<UInt32>()
            for _ in 0..<Int(count) {
                guard let rawID = reader.unsigned(bits: 32),
                      let width = reader.unsigned(bits: 16), width > 0,
                      let height = reader.unsigned(bits: 16), height > 0,
                      let primary = reader.unsigned(bits: 1),
                      identifiers.insert(UInt32(rawID)).inserted else { return nil }
                displays.append(SessionDisplay(id: UInt32(rawID), width: UInt16(width), height: UInt16(height), primary: primary == 1))
            }
            guard let active = reader.unsigned(bits: 32), identifiers.contains(UInt32(active)), reader.isAtEnd else { return nil }
            self = .displays(displays, activeID: UInt32(active))
        case 7:
            guard let available = reader.unsigned(bits: 1), let enabled = reader.unsigned(bits: 1), reader.isAtEnd else { return nil }
            self = .audio(available: available == 1, enabled: enabled == 1)
        case 10:
            guard let rawPreference = reader.unsigned(bits: 32),
                  let preference = SessionVideoQualityPreference(rawValue: UInt32(rawPreference)),
                  let rawPreset = reader.unsigned(bits: 32),
                  let preset = SessionVideoQualityPreset(rawValue: UInt32(rawPreset)), reader.isAtEnd else { return nil }
            self = .videoQuality(SessionVideoQuality(preference: preference, preset: preset))
        case 14:
            guard let present = reader.unsigned(bits: 1) else { return nil }
            if present == 0 {
                guard reader.isAtEnd else { return nil }
                self = .inputRestriction(nil)
            } else {
                guard let rawReason = reader.unsigned(bits: 32),
                      let reason = SessionInputRestrictionReason(rawValue: UInt32(rawReason)), reader.isAtEnd else { return nil }
                self = .inputRestriction(reason)
            }
        default: return nil
        }
    }
}

private struct SessionControlReader {
    let bytes: [UInt8]
    private var offset = 0
    var isAtEnd: Bool { offset == bytes.count }

    init(bytes: [UInt8]) { self.bytes = bytes }

    mutating func unsigned(bits: Int) -> UInt64? {
        var value: UInt64 = 0
        for index in 0..<((bits + 6) / 7) {
            guard offset < bytes.count else { return nil }
            let byte = bytes[offset]
            offset += 1
            let payload = UInt64(byte & 0x7f)
            let shift = index * 7
            let remaining = min(7, bits - shift)
            guard payload < (UInt64(1) << remaining) else { return nil }
            value |= payload << shift
            if byte & 0x80 == 0 {
                guard index == 0 || payload != 0 else { return nil }
                return value
            }
        }
        return nil
    }
}
