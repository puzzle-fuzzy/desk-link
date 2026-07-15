import AppKit

enum KeyboardMapper {
    static func map(
        keyCode: UInt16,
        characters: String?,
        modifiers: NSEvent.ModifierFlags,
        isDown: Bool
    ) -> [MacInputCommand] {
        let mappedModifiers = Modifiers(appKit: modifiers)
        if let logicalKeyCode = MacKeyCodeMapper.protocolCode(forAppKitKeyCode: keyCode) {
            return [.key(code: logicalKeyCode, pressed: isDown, modifiers: mappedModifiers)]
        }
        guard isDown, let characters, !characters.isEmpty else { return [] }
        return [.unicode(characters, modifiers: mappedModifiers)]
    }

}

enum MacKeyCodeMapper {
    static func protocolCode(forAppKitKeyCode keyCode: UInt16) -> UInt32? {
        switch keyCode {
        case 0x24, 0x4c: return 1 // Return / keypad Enter
        case 0x35: return 2 // Escape
        case 0x33: return 3 // Delete / Backspace
        case 0x30: return 4 // Tab
        case 0x7e: return 5 // Arrow up
        case 0x7d: return 6 // Arrow down
        case 0x7b: return 7 // Arrow left
        case 0x7c: return 8 // Arrow right
        default: return nil
        }
    }

    static func appKitKeyCode(forProtocolCode code: UInt32) -> UInt32? {
        switch code {
        case 1: return 0x24 // Return
        case 2: return 0x35 // Escape
        case 3: return 0x33 // Delete / Backspace
        case 4: return 0x30 // Tab
        case 5: return 0x7e // Arrow up
        case 6: return 0x7d // Arrow down
        case 7: return 0x7b // Arrow left
        case 8: return 0x7c // Arrow right
        default: return nil
        }
    }
}

private extension Modifiers {
    init(appKit flags: NSEvent.ModifierFlags) {
        var result: Modifiers = []
        if flags.contains(.shift) { result.insert(.shift) }
        if flags.contains(.control) { result.insert(.control) }
        if flags.contains(.option) { result.insert(.option) }
        if flags.contains(.command) { result.insert(.meta) }
        self = result
    }
}
