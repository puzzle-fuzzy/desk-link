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
        case 0x75: return 9 // Forward delete
        case 0x72: return 10 // Help / Insert
        case 0x73: return 11 // Home
        case 0x77: return 12 // End
        case 0x74: return 13 // Page up
        case 0x79: return 14 // Page down
        case 0x39: return 15 // Caps lock
        case 0x7a: return 16 // F1
        case 0x78: return 17 // F2
        case 0x63: return 18 // F3
        case 0x76: return 19 // F4
        case 0x60: return 20 // F5
        case 0x61: return 21 // F6
        case 0x62: return 22 // F7
        case 0x64: return 23 // F8
        case 0x65: return 24 // F9
        case 0x6d: return 25 // F10
        case 0x67: return 26 // F11
        case 0x6f: return 27 // F12
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
        case 9: return 0x75 // Forward delete
        case 10: return 0x72 // Help / Insert
        case 11: return 0x73 // Home
        case 12: return 0x77 // End
        case 13: return 0x74 // Page up
        case 14: return 0x79 // Page down
        case 15: return 0x39 // Caps lock
        case 16: return 0x7a // F1
        case 17: return 0x78 // F2
        case 18: return 0x63 // F3
        case 19: return 0x76 // F4
        case 20: return 0x60 // F5
        case 21: return 0x61 // F6
        case 22: return 0x62 // F7
        case 23: return 0x64 // F8
        case 24: return 0x65 // F9
        case 25: return 0x6d // F10
        case 26: return 0x67 // F11
        case 27: return 0x6f // F12
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
