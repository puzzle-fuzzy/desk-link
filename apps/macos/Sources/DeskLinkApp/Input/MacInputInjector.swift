import CoreGraphics
import Foundation

struct Modifiers: OptionSet, Equatable, Sendable {
    let rawValue: UInt32

    static let shift = Modifiers(rawValue: 1 << 0)
    static let control = Modifiers(rawValue: 1 << 1)
    static let option = Modifiers(rawValue: 1 << 2)
    static let meta = Modifiers(rawValue: 1 << 3)
    static let capsLock = Modifiers(rawValue: 1 << 4)
}

enum MouseButton: UInt32, CaseIterable, Comparable, Sendable {
    case left = 0
    case right = 1
    case center = 2

    static func < (lhs: MouseButton, rhs: MouseButton) -> Bool { lhs.rawValue < rhs.rawValue }
}

enum MacInputCommand: Equatable, Sendable {
    case move(normalizedX: Float, normalizedY: Float)
    case mouseButton(_ button: MouseButton, pressed: Bool)
    case wheel(deltaX: Int32, deltaY: Int32)
    case key(code: UInt32, pressed: Bool, modifiers: Modifiers)
    case unicode(String, modifiers: Modifiers)
}

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
    func postUnicode(_ text: String, modifiers: Modifiers) throws
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
        event.flags = cgFlags(for: modifiers)
        event.post(tap: .cghidEventTap)
    }

    func postUnicode(_ text: String, modifiers: Modifiers) throws {
        guard !text.isEmpty, let event = CGEvent(source: nil) else {
            throw MacInputInjectorError.eventCreationFailed
        }
        let characters = Array(text.utf16)
        event.flags = cgFlags(for: modifiers)
        characters.withUnsafeBufferPointer {
            event.keyboardSetUnicodeString(stringLength: characters.count, unicodeString: $0.baseAddress!)
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
    private var pointer: CGPoint
    private var pressedKeys = Set<UInt32>()
    private var pressedButtons = Set<MouseButton>()

    var pressedInputs: Set<PressedInput> {
        Set(pressedKeys.map(PressedInput.key)).union(pressedButtons.map(PressedInput.button))
    }

    init(
        backend: any CGEventBackend = SystemCGEventBackend(),
        displayFrame: CGRect = CGDisplayBounds(CGMainDisplayID())
    ) {
        self.backend = backend
        self.displayFrame = displayFrame
        pointer = CGPoint(x: displayFrame.midX, y: displayFrame.midY)
    }

    func inject(_ command: MacInputCommand) throws {
        switch command {
        case let .move(normalizedX, normalizedY):
            guard normalizedX.isFinite, normalizedY.isFinite,
                  (0...1).contains(normalizedX), (0...1).contains(normalizedY)
            else { throw MacInputInjectorError.invalidCoordinate }
            let point = CGPoint(
                x: displayFrame.minX + displayFrame.width * CGFloat(normalizedX),
                y: displayFrame.minY + displayFrame.height * CGFloat(normalizedY)
            )
            try backend.moveMouse(to: point)
            pointer = point
        case let .mouseButton(button, pressed):
            try backend.postMouseButton(button, pressed: pressed, at: pointer)
            if pressed { pressedButtons.insert(button) } else { pressedButtons.remove(button) }
        case let .wheel(deltaX, deltaY):
            try backend.postScroll(deltaX: deltaX, deltaY: deltaY)
        case let .key(code, pressed, modifiers):
            try backend.postKey(code: code, pressed: pressed, modifiers: modifiers)
            if pressed { pressedKeys.insert(code) } else { pressedKeys.remove(code) }
        case let .unicode(text, modifiers):
            try backend.postUnicode(text, modifiers: modifiers)
        }
    }

    func releaseAll() {
        let keys = pressedKeys.sorted()
        let buttons = pressedButtons.sorted()
        for key in keys { try? backend.postKey(code: key, pressed: false, modifiers: []) }
        for button in buttons { try? backend.postMouseButton(button, pressed: false, at: pointer) }
        pressedKeys.removeAll()
        pressedButtons.removeAll()
    }
}

enum PressedInput: Hashable {
    case key(UInt32)
    case button(MouseButton)
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
