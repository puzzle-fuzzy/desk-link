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

    var displayTitle: String {
        switch self {
        case .option: "Alt"
        case .command: "⌘ Win"
        default: rawValue
        }
    }

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
    @Published private(set) var isKeyboardVisible = false

    private let bridge: ControllerBridge
    private var responder: IOSKeyboardResponder?
    private var pressedKeys: [IOSSpecialKey: CapturedRemoteInput] = [:]
    private var surfaceContext: CapturedInputContext?

    init(bridge: ControllerBridge) {
        self.bridge = bridge
        surfaceContext = bridge.presentedInputContext
    }

    func makeResponderView() -> IOSKeyboardResponder {
        synchronizeInputContext(bridge.presentedInputContext)
        if let responder { return responder }
        let responder = IOSKeyboardResponder()
        responder.owner = self
        self.responder = responder
        return responder
    }

    func becomeFirstResponder() {
        guard let context = bridge.captureInputContext() else { return }
        synchronizeInputContext(context)
        let responder = makeResponderView()
        responder.capturedTextContext = context
        isKeyboardVisible = responder.becomeFirstResponder()
    }

    func resign() {
        guard let responder, responder.isFirstResponder else {
            finishResigning()
            return
        }
        _ = responder.resignFirstResponder()
    }

    fileprivate func responderDidResign(_ source: IOSKeyboardResponder) {
        guard responder === source else { return }
        finishResigning()
    }

    private func finishResigning() {
        let contexts = Set(pressedKeys.values.map(\.context)).union([responder?.capturedTextContext].compactMap { $0 })
        responder?.capturedTextContext = nil
        isKeyboardVisible = false
        activeModifiers = []
        pressedKeys.removeAll()
        for context in contexts { bridge.releaseAll(context: context) }
    }

    func synchronizeInputContext(_ context: CapturedInputContext?) {
        guard context?.targetIdentity != surfaceContext?.targetIdentity else { return }
        // Retire the responder too: a delayed UIKit composition callback from
        // the old field must not inherit a freshly focused field's context.
        finishResigning()
        let retired = responder
        responder = nil
        retired?.owner = nil
        _ = retired?.resignFirstResponder()
        surfaceContext = context
    }

    func sendSpecialKey(_ key: IOSSpecialKey, pressed: Bool) {
        if !pressed {
            guard let original = pressedKeys.removeValue(forKey: key),
                  case let .key(code, _, modifiers) = original.command else { return }
            bridge.send(input: .key(code: code, pressed: false, modifiers: modifiers), context: original.context)
            if let modifier = key.modifier { activeModifiers.remove(modifier) }
            return
        }
        guard let context = bridge.captureInputContext() else { return }
        synchronizeInputContext(context)
        guard pressedKeys[key] == nil else { return }
        let captured = CapturedRemoteInput(.key(code: key.code, pressed: true, modifiers: key.modifier == nil ? activeModifiers : []), context: context)
        pressedKeys[key] = captured
        bridge.send(captured: captured)
        if let modifier = key.modifier { activeModifiers.insert(modifier) }
    }

    fileprivate func sendCommittedText(_ text: String, context: CapturedInputContext) {
        guard !text.isEmpty else { return }
        bridge.send(input: .unicode(text, modifiers: activeModifiers), context: context)
    }

    static func isReturnText(_ text: String) -> Bool {
        text == "\n" || text == "\r" || text == "\r\n"
    }

    fileprivate func sendBackspace(context: CapturedInputContext) {
        bridge.send(input: .key(code: IOSSpecialKey.backspace.code, pressed: true, modifiers: activeModifiers), context: context)
        bridge.send(input: .key(code: IOSSpecialKey.backspace.code, pressed: false, modifiers: activeModifiers), context: context)
    }

    fileprivate func sendCapturedSpecialKey(_ key: IOSSpecialKey, context: CapturedInputContext) {
        bridge.send(input: .key(code: key.code, pressed: true, modifiers: activeModifiers), context: context)
        bridge.send(input: .key(code: key.code, pressed: false, modifiers: activeModifiers), context: context)
    }
}

@MainActor
final class IOSKeyboardResponder: UITextView {
    weak var owner: IOSKeyboardInput?
    var capturedTextContext: CapturedInputContext?

    override var canBecomeFirstResponder: Bool { true }

    override init(frame: CGRect, textContainer: NSTextContainer?) {
        super.init(frame: frame, textContainer: textContainer)
        inputAccessoryView = Self.makeInputAccessoryView(target: self)
    }

    required init?(coder: NSCoder) {
        super.init(coder: coder)
        inputAccessoryView = Self.makeInputAccessoryView(target: self)
    }

    private static func makeInputAccessoryView(target: IOSKeyboardResponder) -> UIToolbar {
        let toolbar = UIToolbar()
        toolbar.sizeToFit()
        toolbar.items = [
            UIBarButtonItem(
                barButtonSystemItem: .flexibleSpace,
                target: nil,
                action: nil
            ),
            UIBarButtonItem(
                title: "收起键盘",
                style: .done,
                target: self,
                action: #selector(dismissKeyboard)
            ),
        ]
        return toolbar
    }

    override func resignFirstResponder() -> Bool {
        let didResign = super.resignFirstResponder()
        if didResign { owner?.responderDidResign(self) }
        return didResign
    }

    @objc private func dismissKeyboard() {
        _ = resignFirstResponder()
    }

    override func insertText(_ text: String) {
        guard let context = capturedTextContext else { return }
        owner?.sendCommittedText(text, context: context)
    }

    override func deleteBackward() {
        guard let context = capturedTextContext else { return }
        owner?.sendBackspace(context: context)
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
        guard let key = Self.specialKey(for: command.input), let context = capturedTextContext else { return }
        owner?.sendCapturedSpecialKey(key, context: context)
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
