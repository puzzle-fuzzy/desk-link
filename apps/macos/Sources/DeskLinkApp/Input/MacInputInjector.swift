import CoreGraphics
import DeskLinkAppleCore
import Foundation

enum MacInputInjectorError: Error, Equatable {
    case invalidCoordinate
    case invalidKeyCode
    case eventCreationFailed
}

protocol CGEventBackend: AnyObject {
    func moveMouse(to point: CGPoint) throws
    func postMouseButton(_ button: MouseButton, pressed: Bool, at point: CGPoint) throws
    func postScroll(deltaX: Int32, deltaY: Int32) throws
    func postKey(code: UInt32, pressed: Bool, modifiers: Modifiers) throws
    func postUnicode(_ text: String, pressed: Bool, modifiers: Modifiers) throws
}

final class SystemCGEventBackend: CGEventBackend {
    func moveMouse(to point: CGPoint) throws {
        try post(mouseEvent(type: .mouseMoved, point: point, button: .left))
    }

    func postMouseButton(_ button: MouseButton, pressed: Bool, at point: CGPoint) throws {
        let type: CGEventType
        switch (button, pressed) {
        case (.left, true): type = .leftMouseDown
        case (.left, false): type = .leftMouseUp
        case (.right, true): type = .rightMouseDown
        case (.right, false): type = .rightMouseUp
        case (.center, true): type = .otherMouseDown
        case (.center, false): type = .otherMouseUp
        }
        guard let cgButton = CGMouseButton(rawValue: button.rawValue) else {
            throw MacInputInjectorError.eventCreationFailed
        }
        try post(mouseEvent(type: type, point: point, button: cgButton))
    }

    func postScroll(deltaX: Int32, deltaY: Int32) throws {
        guard let event = CGEvent(
            scrollWheelEvent2Source: nil,
            units: .pixel,
            wheelCount: 2,
            wheel1: deltaY,
            wheel2: deltaX,
            wheel3: 0
        ) else { throw MacInputInjectorError.eventCreationFailed }
        event.post(tap: .cghidEventTap)
    }

    func postKey(code: UInt32, pressed: Bool, modifiers: Modifiers) throws {
        guard let virtualKey = CGKeyCode(exactly: code),
              let event = CGEvent(keyboardEventSource: nil, virtualKey: virtualKey, keyDown: pressed)
        else { throw MacInputInjectorError.invalidKeyCode }
        event.flags = macKeyboardEventFlags(keyCode: code, pressed: pressed, modifiers: modifiers)
        event.post(tap: .cghidEventTap)
    }

    func postUnicode(_ text: String, pressed: Bool, modifiers: Modifiers) throws {
        guard !text.isEmpty,
              let event = CGEvent(keyboardEventSource: nil, virtualKey: 0, keyDown: pressed)
        else {
            throw MacInputInjectorError.eventCreationFailed
        }
        let characters = Array(text.utf16)
        event.flags = cgFlags(for: modifiers)
        characters.withUnsafeBufferPointer {
            event.keyboardSetUnicodeString(
                stringLength: characters.count,
                unicodeString: $0.baseAddress!
            )
        }
        event.post(tap: .cghidEventTap)
    }

    private func mouseEvent(type: CGEventType, point: CGPoint, button: CGMouseButton) -> CGEvent? {
        CGEvent(mouseEventSource: nil, mouseType: type, mouseCursorPosition: point, mouseButton: button)
    }

    private func post(_ event: CGEvent?) throws {
        guard let event else { throw MacInputInjectorError.eventCreationFailed }
        event.post(tap: .cghidEventTap)
    }
}

final class MacInputInjector {
    private let backend: any CGEventBackend
    private let displayFrame: CGRect
    private let lock = NSLock()
    private var pointer: CGPoint
    private var pressedKeys = Set<UInt32>()
    private var pressedCharacters: [UInt32: Modifiers] = [:]
    private var pressedButtons = Set<MouseButton>()

    var pressedInputs: Set<PressedInput> {
        lock.lock()
        defer { lock.unlock() }
        return Set(pressedKeys.map(PressedInput.key))
            .union(pressedCharacters.keys.map(PressedInput.character))
            .union(pressedButtons.map(PressedInput.button))
    }

    init(
        backend: any CGEventBackend = SystemCGEventBackend(),
        displayFrame: CGRect = CGDisplayBounds(CGMainDisplayID())
    ) {
        self.backend = backend
        self.displayFrame = displayFrame
        pointer = CGPoint(x: displayFrame.midX, y: displayFrame.midY)
    }

    func inject(_ command: RemoteInputCommand) throws {
        lock.lock()
        defer { lock.unlock() }
        switch command {
        case let .move(normalizedX, normalizedY):
            guard normalizedX.isFinite, normalizedY.isFinite,
                  displayFrame.width > 0, displayFrame.height > 0,
                  (0...1).contains(normalizedX), (0...1).contains(normalizedY)
            else { throw MacInputInjectorError.invalidCoordinate }
            let maximumX = displayFrame.maxX.nextDown
            let maximumY = displayFrame.maxY.nextDown
            let point = CGPoint(
                x: min(maximumX, max(displayFrame.minX, displayFrame.minX + displayFrame.width * CGFloat(normalizedX))),
                y: min(maximumY, max(displayFrame.minY, displayFrame.minY + displayFrame.height * CGFloat(normalizedY)))
            )
            try backend.moveMouse(to: point)
            pointer = point
        case let .mouseButton(button, pressed):
            try backend.postMouseButton(button, pressed: pressed, at: pointer)
            if pressed { pressedButtons.insert(button) } else { pressedButtons.remove(button) }
        case let .wheel(deltaX, deltaY):
            try backend.postScroll(deltaX: deltaX, deltaY: deltaY)
        case let .key(code, pressed, modifiers):
            // Controller modifier edges are independent of a special key's
            // up packet. Preserve their flags on macOS as well as Windows.
            var effectiveModifiers = modifiers
            for heldKey in pressedKeys {
                if let modifier = modifier(forVirtualKey: heldKey) { effectiveModifiers.insert(modifier) }
            }
            if let modifier = modifier(forVirtualKey: code) {
                if pressed { effectiveModifiers.insert(modifier) }
                else { effectiveModifiers.remove(modifier) }
            }
            try backend.postKey(code: code, pressed: pressed, modifiers: effectiveModifiers)
            if pressed { pressedKeys.insert(code) } else { pressedKeys.remove(code) }
        case let .character(value, pressed, modifiers):
            guard let scalar = UnicodeScalar(value) else { throw MacInputInjectorError.invalidKeyCode }
            try backend.postUnicode(String(scalar), pressed: pressed, modifiers: modifiers)
            if pressed { pressedCharacters[value] = modifiers } else { pressedCharacters.removeValue(forKey: value) }
        case let .unicode(text, modifiers):
            try backend.postUnicode(text, pressed: true, modifiers: modifiers)
            try backend.postUnicode(text, pressed: false, modifiers: modifiers)
        }
    }

    private func modifier(forVirtualKey code: UInt32) -> Modifiers? {
        switch code {
        case 0x3b, 0x3e: .control
        case 0x3a, 0x3d: .option
        case 0x38, 0x3c: .shift
        case 0x37, 0x36: .meta
        default: nil
        }
    }

    @discardableResult
    func releaseAll() -> [MacInputInjectorError] {
        lock.lock()
        defer { lock.unlock() }
        let keys = pressedKeys.sorted()
        let characters = pressedCharacters.keys.sorted()
        let buttons = pressedButtons.sorted()
        var failures: [MacInputInjectorError] = []
        for value in characters {
            do {
                guard let scalar = UnicodeScalar(value) else { throw MacInputInjectorError.invalidKeyCode }
                try backend.postUnicode(String(scalar), pressed: false, modifiers: pressedCharacters[value] ?? [])
                pressedCharacters.removeValue(forKey: value)
            } catch let error as MacInputInjectorError {
                failures.append(error)
            } catch {
                failures.append(.eventCreationFailed)
            }
        }
        for key in keys {
            do {
                try backend.postKey(code: key, pressed: false, modifiers: [])
                pressedKeys.remove(key)
            } catch let error as MacInputInjectorError {
                failures.append(error)
            } catch {
                failures.append(.eventCreationFailed)
            }
        }
        for button in buttons {
            do {
                try backend.postMouseButton(button, pressed: false, at: pointer)
                pressedButtons.remove(button)
            } catch let error as MacInputInjectorError {
                failures.append(error)
            } catch {
                failures.append(.eventCreationFailed)
            }
        }
        return failures
    }
}

enum PressedInput: Hashable {
    case key(UInt32)
    case character(UInt32)
    case button(MouseButton)
}

func macKeyboardEventFlags(keyCode: UInt32, pressed: Bool, modifiers: Modifiers) -> CGEventFlags {
    var flags = cgFlags(for: modifiers)
    let ownModifier: CGEventFlags?
    switch keyCode {
    case 0x36, 0x37: ownModifier = .maskCommand
    case 0x38, 0x3c: ownModifier = .maskShift
    case 0x3a, 0x3d: ownModifier = .maskAlternate
    case 0x3b, 0x3e: ownModifier = .maskControl
    default: ownModifier = nil
    }
    if let ownModifier {
        if pressed { flags.insert(ownModifier) } else { flags.remove(ownModifier) }
    }
    return flags
}

private func cgFlags(for modifiers: Modifiers) -> CGEventFlags {
    var flags: CGEventFlags = []
    if modifiers.contains(.shift) { flags.insert(.maskShift) }
    if modifiers.contains(.control) { flags.insert(.maskControl) }
    if modifiers.contains(.option) { flags.insert(.maskAlternate) }
    if modifiers.contains(.meta) { flags.insert(.maskCommand) }
    if modifiers.contains(.capsLock) { flags.insert(.maskAlphaShift) }
    return flags
}
