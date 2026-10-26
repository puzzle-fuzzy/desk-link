import AppKit
import DeskLinkAppleCore

struct KeyboardMappingPolicy: Equatable {
    var mode: ModifierMode = .automatic
    var remotePlatform: RemotePlatform = .unknown

    var mapsCommandToControl: Bool { mode == .automatic && remotePlatform == .windows }

    func modifiers(for flags: NSEvent.ModifierFlags) -> Modifiers {
        var result: Modifiers = []
        if flags.contains(.shift) { result.insert(.shift) }
        if flags.contains(.control) { result.insert(.control) }
        if flags.contains(.option) { result.insert(.option) }
        if flags.contains(.command) { result.insert(mapsCommandToControl ? .control : .meta) }
        return result
    }
}
