import Combine
import DeskLinkAppleCore
import SwiftUI
import UIKit

enum IOSSpecialKey: String, CaseIterable, Hashable, Identifiable {
    case escape = "Esc"
    case tab = "Tab"
    case enter = "Enter"
    case backspace = "⌫"
    case control = "Ctrl"
    case option = "Option"
    case command = "Command / Win"
    case shift = "Shift"
    case arrowUp = "↑"
    case arrowDown = "↓"
    case arrowLeft = "←"
    case arrowRight = "→"

    var id: Self { self }

    var code: UInt32 {
        switch self {
        case .escape: DeskLinkKeyCode.escape
        case .tab: DeskLinkKeyCode.tab
        case .enter: DeskLinkKeyCode.enter
        case .backspace: DeskLinkKeyCode.backspace
        case .control: DeskLinkKeyCode.control
        case .option: DeskLinkKeyCode.alt
        case .command: DeskLinkKeyCode.meta
        case .shift: DeskLinkKeyCode.shift
        case .arrowUp: DeskLinkKeyCode.arrowUp
        case .arrowDown: DeskLinkKeyCode.arrowDown
        case .arrowLeft: DeskLinkKeyCode.arrowLeft
        case .arrowRight: DeskLinkKeyCode.arrowRight
        }
    }

    var modifier: Modifiers? {
        switch self {
        case .control: .control
        case .option: .option
        case .command: .meta
        case .shift: .shift
        default: nil
        }
    }
}

@MainActor
final class IOSKeyboardInput: ObservableObject {
    @Published private(set) var activeModifiers: Modifiers = []

    private let bridge: ControllerBridge
    private var responder: IOSKeyboardResponder?

    init(bridge: ControllerBridge) {
        self.bridge = bridge
    }

    func makeResponderView() -> IOSKeyboardResponder {
        if let responder { return responder }
        let responder = IOSKeyboardResponder()
        responder.owner = self
        self.responder = responder
        return responder
    }

    func becomeFirstResponder() {
        makeResponderView().becomeFirstResponder()
    }

    func resign() {
        responder?.resignFirstResponder()
        bridge.releaseAll()
        activeModifiers = []
    }

    func sendSpecialKey(_ key: IOSSpecialKey, pressed: Bool) {
        if let modifier = key.modifier {
            bridge.send(input: .key(code: key.code, pressed: pressed, modifiers: []))
            if pressed {
                activeModifiers.insert(modifier)
            } else {
                activeModifiers.remove(modifier)
            }
            return
        }

        bridge.send(input: .key(code: key.code, pressed: pressed, modifiers: activeModifiers))
    }

    fileprivate func sendCommittedText(_ text: String) {
        guard !text.isEmpty else { return }
        bridge.send(input: .unicode(text, modifiers: activeModifiers))
    }

    fileprivate func sendBackspace() {
        sendSpecialKey(.backspace, pressed: true)
        sendSpecialKey(.backspace, pressed: false)
    }
}

@MainActor
final class IOSKeyboardResponder: UITextView {
    weak var owner: IOSKeyboardInput?

    override var canBecomeFirstResponder: Bool { true }

    override func insertText(_ text: String) {
        owner?.sendCommittedText(text)
    }

    override func deleteBackward() {
        owner?.sendBackspace()
    }

    override var keyCommands: [UIKeyCommand]? {
        [
            UIKeyCommand(input: UIKeyCommand.inputUpArrow, modifierFlags: [], action: #selector(handleKeyCommand(_:))),
            UIKeyCommand(input: UIKeyCommand.inputDownArrow, modifierFlags: [], action: #selector(handleKeyCommand(_:))),
            UIKeyCommand(input: UIKeyCommand.inputLeftArrow, modifierFlags: [], action: #selector(handleKeyCommand(_:))),
            UIKeyCommand(input: UIKeyCommand.inputRightArrow, modifierFlags: [], action: #selector(handleKeyCommand(_:))),
            UIKeyCommand(input: UIKeyCommand.inputEscape, modifierFlags: [], action: #selector(handleKeyCommand(_:))),
            UIKeyCommand(input: "\t", modifierFlags: [], action: #selector(handleKeyCommand(_:))),
            UIKeyCommand(input: UIKeyCommand.inputDelete, modifierFlags: [], action: #selector(handleKeyCommand(_:))),
            UIKeyCommand(input: "\r", modifierFlags: [], action: #selector(handleKeyCommand(_:))),
        ]
    }

    @objc private func handleKeyCommand(_ command: UIKeyCommand) {
        guard let key = Self.specialKey(for: command.input) else { return }
        owner?.sendSpecialKey(key, pressed: true)
        owner?.sendSpecialKey(key, pressed: false)
    }

    private static func specialKey(for input: String?) -> IOSSpecialKey? {
        switch input {
        case UIKeyCommand.inputUpArrow: .arrowUp
        case UIKeyCommand.inputDownArrow: .arrowDown
        case UIKeyCommand.inputLeftArrow: .arrowLeft
        case UIKeyCommand.inputRightArrow: .arrowRight
        case UIKeyCommand.inputEscape: .escape
        case "\t": .tab
        case UIKeyCommand.inputDelete: .backspace
        case "\r": .enter
        default: nil
        }
    }
}

struct IOSKeyboardInputView: UIViewRepresentable {
    @ObservedObject var input: IOSKeyboardInput

    func makeUIView(context: Context) -> IOSKeyboardResponder {
        let view = input.makeResponderView()
        view.backgroundColor = .clear
        view.alpha = 0.01
        view.textColor = .clear
        view.tintColor = .clear
        view.isScrollEnabled = false
        return view
    }

    func updateUIView(_ view: IOSKeyboardResponder, context: Context) {}
}

@MainActor
final class TestableIOSKeyboardInput {
    private(set) var releaseAllCallCount = 0
    private(set) var commands: [IOSSpecialKey] = []

    func sendSpecialKey(_ key: IOSSpecialKey, pressed: Bool) {
        if pressed { commands.append(key) }
    }

    func resign() {
        releaseAllCallCount += 1
        commands.removeAll()
    }
}
