import AppKit

enum KeyboardMapper {
    static func map(
        keyCode: UInt16,
        characters: String?,
        modifiers: NSEvent.ModifierFlags,
        isDown: Bool
    ) -> [MacInputCommand] {
        let mappedModifiers = Modifiers(appKit: modifiers)
        var commands: [MacInputCommand] = [
            .key(code: UInt32(keyCode), pressed: isDown, modifiers: mappedModifiers),
        ]
        if isDown, let characters, !characters.isEmpty {
            commands.append(.unicode(characters, modifiers: mappedModifiers))
        }
        return commands
    }
}

private extension Modifiers {
    init(appKit flags: NSEvent.ModifierFlags) {
        var result: Modifiers = []
        if flags.contains(.shift) { result.insert(.shift) }
        if flags.contains(.control) { result.insert(.control) }
        if flags.contains(.option) { result.insert(.option) }
        if flags.contains(.command) { result.insert(.meta) }
        if flags.contains(.capsLock) { result.insert(.capsLock) }
        self = result
    }
}
