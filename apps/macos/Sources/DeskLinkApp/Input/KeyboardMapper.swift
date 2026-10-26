import AppKit
import DeskLinkAppleCore

enum KeyboardMapper {
    static func map(
        keyCode: UInt16,
        characters: String?,
        modifiers: NSEvent.ModifierFlags,
        isDown: Bool
    ) -> [RemoteInputCommand] {
        let mappedModifiers = Modifiers(appKit: modifiers)
        if let logicalKeyCode = MacKeyCodeMapper.protocolCode(forAppKitKeyCode: keyCode) {
            return [.key(code: logicalKeyCode, pressed: isDown, modifiers: mappedModifiers)]
        }
        guard let characters, !characters.isEmpty else { return [] }
        return characters.unicodeScalars
            .filter { !CharacterSet.controlCharacters.contains($0) }
            .map { .character($0.value, pressed: isDown, modifiers: mappedModifiers) }
    }

}

// A key's release must use its original character and modifiers. AppKit may
// report different characters after Shift, the keyboard layout, or IME changes.
struct KeyboardInputState {
    private var pressedKeys: [UInt16: [RemoteInputCommand]] = [:]
    private var heldModifiers: Modifiers = []
    private var capsLockEnabled = false

    mutating func keyDown(
        keyCode: UInt16,
        characters: String?,
        modifiers: NSEvent.ModifierFlags,
        isRepeat: Bool
    ) -> [RemoteInputCommand] {
        if let held = pressedKeys[keyCode] { return isRepeat ? held : [] }
        let commands = KeyboardMapper.map(
            keyCode: keyCode, characters: characters, modifiers: modifiers, isDown: true
        )
        if !commands.isEmpty { pressedKeys[keyCode] = commands }
        return commands
    }

    mutating func keyUp(keyCode: UInt16) -> [RemoteInputCommand] {
        guard let held = pressedKeys.removeValue(forKey: keyCode) else { return [] }
        let releases = held.reversed().compactMap { command -> RemoteInputCommand? in
            switch command {
            case let .key(code, _, modifiers): .key(code: code, pressed: false, modifiers: modifiers)
            case let .character(scalar, _, modifiers): .character(scalar, pressed: false, modifiers: modifiers)
            default: nil
            }
        }
        // The protocol's chord key-up also releases its modifier mask. Restore
        // modifiers still physically held for a subsequent drag or key chord.
        let chordModifiers = held.reduce(Modifiers()) { result, command in
            switch command {
            case let .key(_, _, modifiers), let .character(_, _, modifiers): result.union(modifiers)
            default: result
            }
        }
        return releases + modifierCommands(chordModifiers.intersection(heldModifiers), pressed: true)
    }

    mutating func flagsChanged(_ flags: NSEvent.ModifierFlags) -> [RemoteInputCommand] {
        let next = Modifiers(appKit: flags)
        var commands = modifierCommands(heldModifiers.subtracting(next), pressed: false)
        commands += modifierCommands(next.subtracting(heldModifiers), pressed: true)
        heldModifiers = next
        let capsLock = flags.contains(.capsLock)
        if capsLock != capsLockEnabled {
            commands += [
                .key(code: DeskLinkKeyCode.capsLock, pressed: true, modifiers: []),
                .key(code: DeskLinkKeyCode.capsLock, pressed: false, modifiers: []),
            ]
            capsLockEnabled = capsLock
        }
        return commands
    }

    mutating func reset() {
        pressedKeys.removeAll()
        heldModifiers = []
        capsLockEnabled = false
    }

    private func modifierCommands(_ flags: Modifiers, pressed: Bool) -> [RemoteInputCommand] {
        let mapping: [(Modifiers, UInt32)] = [
            (.control, DeskLinkKeyCode.control), (.option, DeskLinkKeyCode.alt),
            (.shift, DeskLinkKeyCode.shift), (.meta, DeskLinkKeyCode.meta),
        ]
        let entries = pressed ? mapping : Array(mapping.reversed())
        return entries.compactMap { flag, code in
            flags.contains(flag) ? .key(code: code, pressed: pressed, modifiers: []) : nil
        }
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
        case 28: return 0x3b // Control
        case 29: return 0x3a // Option
        case 30: return 0x38 // Shift
        case 31: return 0x37 // Command
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
