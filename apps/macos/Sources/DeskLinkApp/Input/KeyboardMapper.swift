import AppKit
import DeskLinkAppleCore

enum KeyboardMapper {
    static func map(
        keyCode: UInt16,
        characters: String?,
        modifiers: NSEvent.ModifierFlags,
        isDown: Bool,
        policy: KeyboardMappingPolicy? = nil,
        modifierMode: ModifierMode = .raw,
        charactersIgnoringModifiers: String? = nil
    ) -> [RemoteInputCommand] {
        // Main's capture-context callers pass a platform-aware policy; the
        // branch's view-level callers pass the UI modifier mode instead.
        let remoteModifiers = policy.map { $0.modifiers(for: modifiers) }
            ?? mappedModifiers(modifiers, mode: modifierMode)
        if let logicalKeyCode = MacKeyCodeMapper.protocolCode(forAppKitKeyCode: keyCode) {
            return [.key(code: logicalKeyCode, pressed: isDown, modifiers: remoteModifiers)]
        }
        // Control-modified NSEvent.characters can be a control character
        // (Control+A is U+0001). Remote shortcuts require the base character.
        let resolved = modifiers.intersection([.command, .control, .option]).isEmpty
            ? characters : (charactersIgnoringModifiers ?? characters)
        if let policy {
            // Policy callers (KeyboardInputState) drive physical key edges:
            // every scalar keeps its pressed state and the original modifier
            // mask so late key-ups still target the identity that went down.
            guard let resolved, !resolved.isEmpty else { return [] }
            return resolved.unicodeScalars
                .filter { !CharacterSet.controlCharacters.contains($0) }
                .map { .character($0.value, pressed: isDown, modifiers: remoteModifiers) }
        }
        guard isDown, let resolved, !resolved.isEmpty else { return [] }
        return [.unicode(resolved, modifiers: remoteModifiers)]
    }

    static func mappedModifiers(_ flags: NSEvent.ModifierFlags,
                                mode: ModifierMode) -> Modifiers {
        var result: Modifiers = []
        if flags.contains(.shift) { result.insert(.shift) }
        if flags.contains(.control) { result.insert(.control) }
        if flags.contains(.option) { result.insert(.option) }
        if flags.contains(.command) {
            result.insert(mode == .automatic ? .control : .meta)
        }
        return result
    }

    static let modifierKeys: [(modifier: Modifiers, code: UInt32)] = [
        (.control, DeskLinkKeyCode.control), (.option, DeskLinkKeyCode.alt),
        (.shift, DeskLinkKeyCode.shift), (.meta, DeskLinkKeyCode.meta),
    ]
}

// A key's release must use its original character and modifiers. AppKit may
// report different characters after Shift, the keyboard layout, or IME changes.
struct KeyboardInputState: Sendable {
    private struct KeyPress: Sendable {
        let context: CapturedInputContext
        let commands: [RemoteInputCommand]
    }
    private var pressedKeys: [UInt16: KeyPress] = [:]
    private var modifierContexts: [UInt32: CapturedInputContext] = [:]
    private var capsLockEnabled: Bool?

    var activeContexts: Set<CapturedInputContext> {
        Set(pressedKeys.values.map(\.context)).union(modifierContexts.values)
    }

    mutating func keyDown(
        context: CapturedInputContext,
        keyCode: UInt16,
        characters: String?,
        modifiers: NSEvent.ModifierFlags,
        isRepeat: Bool,
        policy: KeyboardMappingPolicy = KeyboardMappingPolicy()
    ) -> [CapturedRemoteInput] {
        let commands = KeyboardMapper.map(
            keyCode: keyCode, characters: characters, modifiers: modifiers, isDown: true, policy: policy
        )
        if let held = pressedKeys[keyCode] {
            guard isRepeat, held.context.hasSameInputTarget(as: context) else { return [] }
            guard commands != held.commands else { return capture(held.commands, context: held.context) }
            let releases = keyUp(keyCode: keyCode)
            if !commands.isEmpty { pressedKeys[keyCode] = KeyPress(context: held.context, commands: commands) }
            return releases + capture(commands, context: held.context)
        }
        // A presentation barrier clears presses. A key still held across that
        // barrier must be physically released and pressed again on the new view.
        guard !isRepeat else { return [] }
        if !commands.isEmpty { pressedKeys[keyCode] = KeyPress(context: context, commands: commands) }
        return capture(commands, context: context)
    }

    mutating func keyUp(keyCode: UInt16) -> [CapturedRemoteInput] {
        guard let held = pressedKeys.removeValue(forKey: keyCode) else { return [] }
        let releases = held.commands.reversed().compactMap { command -> RemoteInputCommand? in
            switch command {
            case let .key(code, _, modifiers): .key(code: code, pressed: false, modifiers: modifiers)
            case let .character(scalar, _, modifiers): .character(scalar, pressed: false, modifiers: modifiers)
            default: nil
            }
        }
        let chordModifiers = held.commands.reduce(Modifiers()) { result, command in
            switch command {
            case let .key(_, _, modifiers), let .character(_, _, modifiers): result.union(modifiers)
            default: result
            }
        }
        let restore = modifierEntries.compactMap { flag, code -> CapturedRemoteInput? in
            guard chordModifiers.contains(flag), let original = modifierContexts[flag.rawValue],
                  original.hasSameInputTarget(as: held.context) else { return nil }
            return CapturedRemoteInput(.key(code: code, pressed: true, modifiers: []), context: original)
        }
        return capture(releases, context: held.context) + restore
    }

    mutating func flagsChanged(context: CapturedInputContext, _ flags: NSEvent.ModifierFlags, keyCode: UInt16? = nil, policy: KeyboardMappingPolicy = KeyboardMappingPolicy()) -> [CapturedRemoteInput] {
        let next = policy.modifiers(for: flags)
        let localTransition: NSEvent.ModifierFlags
        switch keyCode {
        case 0x36, 0x37: localTransition = .command
        case 0x38, 0x3c: localTransition = .shift
        case 0x3a, 0x3d: localTransition = .option
        case 0x3b, 0x3e: localTransition = .control
        default: localTransition = []
        }
        let additions = keyCode == nil ? next : policy.modifiers(for: localTransition)
        var captured: [CapturedRemoteInput] = []
        for (flag, code) in modifierEntries.reversed() {
            if !next.contains(flag), let original = modifierContexts.removeValue(forKey: flag.rawValue) {
                captured.append(CapturedRemoteInput(.key(code: code, pressed: false, modifiers: []), context: original))
            }
        }
        for (flag, code) in modifierEntries where next.contains(flag) && additions.contains(flag) && modifierContexts[flag.rawValue] == nil {
            modifierContexts[flag.rawValue] = context
            captured.append(CapturedRemoteInput(.key(code: code, pressed: true, modifiers: []), context: context))
        }
        let capsLock = flags.contains(.capsLock)
        if keyCode == 0x39, capsLockEnabled == nil || capsLock != capsLockEnabled {
            captured += capture([
                .key(code: DeskLinkKeyCode.capsLock, pressed: true, modifiers: []),
                .key(code: DeskLinkKeyCode.capsLock, pressed: false, modifiers: []),
            ], context: context)
        }
        capsLockEnabled = capsLock
        return captured
    }

    mutating func reset() {
        pressedKeys.removeAll()
        modifierContexts.removeAll()
        capsLockEnabled = nil
    }

    private var modifierEntries: [(Modifiers, UInt32)] {
        [(.control, DeskLinkKeyCode.control), (.option, DeskLinkKeyCode.alt),
         (.shift, DeskLinkKeyCode.shift), (.meta, DeskLinkKeyCode.meta)]
    }

    private func capture(_ commands: [RemoteInputCommand], context: CapturedInputContext) -> [CapturedRemoteInput] {
        commands.map { CapturedRemoteInput($0, context: context) }
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
        case 0x3b, 0x3e: return 28 // Left / right Control
        case 0x3a, 0x3d: return 29 // Left / right Option (Alt)
        case 0x38, 0x3c: return 30 // Left / right Shift
        case 0x37, 0x36: return 31 // Left / right Command (Windows key)
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
