import AppKit
import DeskLinkAppleCore
import SwiftUI

// Keep notification-token cleanup outside NSView's nonisolated deinitializer.
// All registration changes are made on the view's main actor.
private final class WindowFocusObservation: @unchecked Sendable {
    var token: NSObjectProtocol?

    func cancel() {
        if let token { NotificationCenter.default.removeObserver(token) }
        token = nil
    }

    deinit { cancel() }
}

struct SessionInputView: NSViewRepresentable {
    @ObservedObject var bridge: ControllerBridge
    let viewport: SessionViewport
    let interactionMode: SessionInteractionMode
    let acceptsRemoteInput: Bool
    let exitFullscreen: () -> Bool
    let onPan: (CGSize) -> Void
    let onPointerActivity: (CGPoint) -> Void
    var modifierMode: ModifierMode = .automatic
    var inputContextID: UInt64 = 0
    var onPasteClipboard: (() -> Void)? = nil
    var onFocusLost: (() -> Void)? = nil

    func makeNSView(context: Context) -> InputView {
        InputView(viewport: viewport, interactionMode: interactionMode,
                  acceptsRemoteInput: acceptsRemoteInput,
                  sendInput: { [weak bridge] in bridge?.send(input: $0) },
                  releaseInput: { [weak bridge] in bridge?.releaseAll() },
                  exitFullscreen: exitFullscreen, onPan: onPan,
                  onPointerActivity: onPointerActivity,
                  modifierMode: modifierMode, inputContextID: inputContextID,
                  onPasteClipboard: onPasteClipboard, onFocusLost: onFocusLost)
    }

    func updateNSView(_ view: InputView, context: Context) {
        view.viewport = viewport
        view.interactionMode = interactionMode
        view.acceptsRemoteInput = acceptsRemoteInput
        view.exitFullscreen = exitFullscreen
        view.onPan = onPan
        view.onPointerActivity = onPointerActivity
        view.modifierMode = modifierMode
        view.inputContextID = inputContextID
        view.onPasteClipboard = onPasteClipboard
        view.onFocusLost = onFocusLost
    }

    static func dismantleNSView(_ view: InputView, coordinator: ()) { view.releaseAll() }

    // NSTextInputClient predates Swift concurrency; AppKit invokes its client
    // callbacks on the UI thread. Keep all NSView state MainActor-isolated.
    final class InputView: NSView, @preconcurrency NSTextInputClient {
        var viewport: SessionViewport
        var interactionMode: SessionInteractionMode {
            didSet {
                if oldValue != interactionMode {
                    onFocusLost?()
                    releaseAll()
                    window?.invalidateCursorRects(for: self)
                }
            }
        }
        var acceptsRemoteInput: Bool {
            didSet { if oldValue && !acceptsRemoteInput { releaseAll() } }
        }
        var exitFullscreen: () -> Bool
        var onPan: (CGSize) -> Void
        var onPointerActivity: (CGPoint) -> Void
        var modifierMode: ModifierMode { didSet { if oldValue != modifierMode { onFocusLost?(); releaseAll() } } }
        var inputContextID: UInt64 { didSet { if oldValue != inputContextID { onFocusLost?(); releaseAll() } } }
        var onPasteClipboard: (() -> Void)?
        var onFocusLost: (() -> Void)?
        private let sendInput: (RemoteInputCommand) -> Void
        private let releaseInput: () -> Void
        private let makeTextInputContext: (any NSTextInputClient) -> NSTextInputContext
        private var pressedButtons = Set<MouseButton>()
        private var suppressedEscapeKeyUp = false
        private var lastDragPoint: CGPoint?
        private var trackingArea: NSTrackingArea?
        private var remoteModifiers: Modifiers = []
        private var physicalKeys = Set<UInt16>()
        private var localKeys = Set<UInt16>()
        private var markedText = NSAttributedString(string: "")
        private var markedSelection = NSRange(location: 0, length: 0)
        private var candidatePoint = CGPoint(x: 20, y: 20)
        private var textInputEnabled = true
        private var textGeneration: UInt64 = 0
        private var textClient: GenerationTextInputClient?
        private var textContext: NSTextInputContext?
        private var currentKeyEvent: NSEvent?
        private var keyBeganInComposition = false
        private var keyProducedOutput = false
        private var keyCommittedText = false

        init(viewport: SessionViewport,
             interactionMode: SessionInteractionMode,
             acceptsRemoteInput: Bool,
             sendInput: @escaping (RemoteInputCommand) -> Void,
             releaseInput: @escaping () -> Void,
             exitFullscreen: @escaping () -> Bool,
             onPan: @escaping (CGSize) -> Void,
             onPointerActivity: @escaping (CGPoint) -> Void = { _ in },
             modifierMode: ModifierMode = .automatic,
             inputContextID: UInt64 = 0,
             onPasteClipboard: (() -> Void)? = nil,
             onFocusLost: (() -> Void)? = nil,
             makeTextInputContext: @escaping (any NSTextInputClient) -> NSTextInputContext = { NSTextInputContext(client: $0) }) {
            self.viewport = viewport
            self.interactionMode = interactionMode
            self.acceptsRemoteInput = acceptsRemoteInput
            self.sendInput = sendInput
            self.releaseInput = releaseInput
            self.exitFullscreen = exitFullscreen
            self.onPan = onPan
            self.onPointerActivity = onPointerActivity
            self.modifierMode = modifierMode
            self.inputContextID = inputContextID
            self.onPasteClipboard = onPasteClipboard
            self.onFocusLost = onFocusLost
            self.makeTextInputContext = makeTextInputContext
            super.init(frame: .zero)
            wantsLayer = true
        }

        required init?(coder: NSCoder) { nil }
        override var isFlipped: Bool { true }
        override var acceptsFirstResponder: Bool { true }
        private var canControl: Bool { interactionMode == .control && acceptsRemoteInput }

        override var inputContext: NSTextInputContext? {
            guard canControl, textInputEnabled else { return nil }
            if textContext == nil {
                // Each input lifetime has its own client. An asynchronous IME
                // callback retained by an old context cannot commit to a new
                // stream, focused view, or interaction mode.
                let client = GenerationTextInputClient(view: self, generation: textGeneration)
                textClient = client
                textContext = makeTextInputContext(client)
            }
            return textContext
        }

        override func becomeFirstResponder() -> Bool {
            let accepted = super.becomeFirstResponder()
            if accepted { textInputEnabled = true }
            return accepted
        }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            NotificationCenter.default.removeObserver(self)
            guard let window else { releaseAll(); return }
            window.acceptsMouseMovedEvents = true
            NotificationCenter.default.addObserver(self, selector: #selector(windowLostFocus),
                                                   name: NSWindow.didResignKeyNotification, object: window)
            NotificationCenter.default.addObserver(self, selector: #selector(windowLostFocus),
                                                   name: NSApplication.didResignActiveNotification, object: nil)
            DispatchQueue.main.async { [weak self] in
                guard let self, self.window?.isKeyWindow == true else { return }
                self.window?.makeFirstResponder(self)
            }
        }

        override func viewWillMove(toWindow newWindow: NSWindow?) {
            if newWindow == nil { onFocusLost?(); releaseAll() }
            super.viewWillMove(toWindow: newWindow)
        }

        override func resignFirstResponder() -> Bool {
            onFocusLost?()
            releaseAll()
            return super.resignFirstResponder()
        }

        override func updateTrackingAreas() {
            super.updateTrackingAreas()
            if let trackingArea { removeTrackingArea(trackingArea) }
            let area = NSTrackingArea(rect: bounds,
                                     options: [.activeInKeyWindow, .inVisibleRect, .mouseMoved],
                                     owner: self)
            addTrackingArea(area)
            trackingArea = area
        }

        override func resetCursorRects() {
            if interactionMode == .browse { addCursorRect(bounds, cursor: .openHand) }
        }

        override func keyDown(with event: NSEvent) {
            if event.keyCode == 0x35, suppressedEscapeKeyUp { return }
            if event.keyCode == 0x35, exitFullscreen() {
                suppressedEscapeKeyUp = true
                releaseAll()
                return
            }
            guard canControl else { return }
            textInputEnabled = true
            let wasComposing = hasMarkedText()
            let shortcut = !event.modifierFlags.intersection([.command, .control, .option]).isEmpty
            if shortcut, !wasComposing {
                if event.keyCode == 0x09,
                   !event.modifierFlags.contains(.option),
                   !event.modifierFlags.contains(.shift),
                   KeyboardMapper.mappedModifiers(event.modifierFlags, mode: modifierMode).contains(.control),
                   let onPasteClipboard {
                    localKeys.insert(event.keyCode)
                    releaseRemoteModifiers()
                    onPasteClipboard()
                    return
                }
                sendKeyboard(event, isDown: true)
                return
            }

            // All printable input (including Latin, dead keys, Chinese and
            // emoji) has exactly one commit route: insertText. Never send the
            // physical spelling of a composition to the remote computer.
            localKeys.insert(event.keyCode)
            currentKeyEvent = event
            keyBeganInComposition = wasComposing
            keyProducedOutput = false
            keyCommittedText = false
            let consumed = inputContext?.handleEvent(event) ?? false
            if !consumed, !keyProducedOutput, !wasComposing, !hasMarkedText() {
                if MacKeyCodeMapper.protocolCode(forAppKitKeyCode: event.keyCode) != nil {
                    sendKeyboard(event, isDown: true)
                } else if let text = event.characters, !text.isEmpty {
                    insertText(text, replacementRange: NSRange(location: NSNotFound, length: 0))
                }
            }
            currentKeyEvent = nil
            keyBeganInComposition = false
        }

        override func keyUp(with event: NSEvent) {
            if event.keyCode == 0x35, suppressedEscapeKeyUp {
                suppressedEscapeKeyUp = false
                return
            }
            if localKeys.remove(event.keyCode) != nil { return }
            guard physicalKeys.remove(event.keyCode) != nil else { return }
            sendKeyboard(event, isDown: false)
        }

        override func performKeyEquivalent(with event: NSEvent) -> Bool {
            guard canControl, window?.firstResponder === self,
                  event.type == .keyDown,
                  !event.modifierFlags.intersection([.command, .control, .option]).isEmpty else { return false }
            keyDown(with: event)
            return true
        }

        override func flagsChanged(with event: NSEvent) {
            guard canControl, textInputEnabled else { return }
            let wasComposing = hasMarkedText()
            _ = inputContext?.handleEvent(event)
            if wasComposing || hasMarkedText() { releaseRemoteModifiers(); return }
            setRemoteModifiers(KeyboardMapper.mappedModifiers(event.modifierFlags, mode: modifierMode))
        }

        override func cancelOperation(_ sender: Any?) {
            if exitFullscreen() { releaseAll() }
            else if hasMarkedText() { cancelComposition() }
        }
        override func mouseMoved(with event: NSEvent) {
            candidatePoint = convert(event.locationInWindow, from: nil)
            onPointerActivity(candidatePoint)
            sendPointer(event)
        }
        override func mouseDragged(with event: NSEvent) {
            if interactionMode == .browse {
                let point = convert(event.locationInWindow, from: nil)
                if let previous = lastDragPoint {
                    onPan(CGSize(width: point.x - previous.x, height: point.y - previous.y))
                }
                lastDragPoint = point
            } else { sendPointer(event) }
        }
        override func rightMouseDragged(with event: NSEvent) { sendPointer(event) }
        override func otherMouseDragged(with event: NSEvent) { sendPointer(event) }

        override func mouseDown(with event: NSEvent) {
            window?.makeFirstResponder(self)
            candidatePoint = convert(event.locationInWindow, from: nil)
            if interactionMode == .browse {
                lastDragPoint = convert(event.locationInWindow, from: nil)
                return
            }
            sendButton(event, pressed: true)
        }
        override func mouseUp(with event: NSEvent) {
            lastDragPoint = nil
            sendButton(event, pressed: false)
        }
        override func rightMouseDown(with event: NSEvent) { sendButton(event, pressed: true) }
        override func rightMouseUp(with event: NSEvent) { sendButton(event, pressed: false) }
        override func otherMouseDown(with event: NSEvent) { sendButton(event, pressed: true) }
        override func otherMouseUp(with event: NSEvent) { sendButton(event, pressed: false) }

        override func scrollWheel(with event: NSEvent) {
            if interactionMode == .browse {
                onPan(CGSize(width: event.scrollingDeltaX, height: event.scrollingDeltaY))
                return
            }
            guard canControl, normalizedPoint(for: event) != nil else { return }
            let deltaX = quantizeScrollDelta(event.scrollingDeltaX)
            let deltaY = quantizeScrollDelta(event.scrollingDeltaY)
            guard deltaX != 0 || deltaY != 0 else { return }
            sendInput(.wheel(deltaX: deltaX, deltaY: deltaY))
        }

        func releaseAll() {
            invalidateTextInput()
            releaseInput()
            remoteModifiers = []
            physicalKeys.removeAll()
            localKeys.removeAll()
            pressedButtons.removeAll()
            lastDragPoint = nil
        }

        @objc private func windowLostFocus(_ notification: Notification) { onFocusLost?(); releaseAll() }

        private func sendKeyboard(_ event: NSEvent, isDown: Bool) {
            guard canControl else { return }
            let logicalKey = MacKeyCodeMapper.protocolCode(forAppKitKeyCode: event.keyCode)
            if isDown {
                if logicalKey != nil { physicalKeys.insert(event.keyCode); localKeys.remove(event.keyCode) }
                else { localKeys.insert(event.keyCode) }
            }
            let restoreModifiers = isDown && logicalKey == nil ? remoteModifiers : nil
            if restoreModifiers != nil { releaseRemoteModifiers() }
            let eventModifiers = KeyboardMapper.mappedModifiers(event.modifierFlags, mode: modifierMode)
            if logicalKey != nil { setRemoteModifiers(eventModifiers) }
            for command in KeyboardMapper.map(keyCode: event.keyCode, characters: event.characters,
                                               modifiers: event.modifierFlags, isDown: isDown,
                                               modifierMode: modifierMode,
                                               charactersIgnoringModifiers: event.charactersIgnoringModifiers) {
                if let logicalKey, !isDown {
                    // The held modifiers already have independent key edges.
                    // A key-up mask would make Windows release Alt in between
                    // consecutive Alt+Tab presses and close its switcher.
                    sendInput(.key(code: logicalKey, pressed: false, modifiers: []))
                } else {
                    sendInput(command)
                }
            }
            if let restoreModifiers { setRemoteModifiers(restoreModifiers) }
        }

        private func setRemoteModifiers(_ desired: Modifiers) {
            // These are physical modifier edges. Piggybacking other modifiers
            // would make the Windows host release them with this key's up edge.
            for (modifier, code) in KeyboardMapper.modifierKeys where remoteModifiers.contains(modifier) && !desired.contains(modifier) {
                remoteModifiers.remove(modifier)
                sendInput(.key(code: code, pressed: false, modifiers: []))
            }
            for (modifier, code) in KeyboardMapper.modifierKeys where !remoteModifiers.contains(modifier) && desired.contains(modifier) {
                remoteModifiers.insert(modifier)
                sendInput(.key(code: code, pressed: true, modifiers: []))
            }
        }

        private func releaseRemoteModifiers() { setRemoteModifiers([]) }

        private func invalidateTextInput() {
            textInputEnabled = false
            textGeneration &+= 1
            clearMarkedText()
            let oldContext = textContext
            textContext = nil
            oldContext?.discardMarkedText()
            oldContext?.deactivate()
            textClient = nil
            currentKeyEvent = nil
        }

        private func cancelComposition() {
            clearMarkedText()
            textContext?.discardMarkedText()
        }

        private func clearMarkedText() {
            markedText = NSAttributedString(string: "")
            markedSelection = NSRange(location: 0, length: 0)
            needsDisplay = true
        }

        // The toolbar's explicit text sender can use this same commit path.
        func sendText(_ text: String) {
            guard canControl, !text.isEmpty else { return }
            let restoreModifiers = remoteModifiers
            releaseRemoteModifiers()
            sendInput(.unicode(text, modifiers: []))
            setRemoteModifiers(restoreModifiers)
        }

        func insertText(_ string: Any, replacementRange: NSRange) {
            guard canControl, textInputEnabled, !keyCommittedText || currentKeyEvent == nil else { return }
            let text = (string as? NSAttributedString)?.string ?? (string as? String) ?? ""
            guard !text.isEmpty else { clearMarkedText(); return }
            keyProducedOutput = true
            keyCommittedText = true
            clearMarkedText()
            sendText(text)
        }

        func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
            guard canControl, textInputEnabled else { return }
            let text = (string as? NSAttributedString) ?? NSAttributedString(string: (string as? String) ?? "")
            markedText = text
            let location = min(text.length, max(0, selectedRange.location == NSNotFound ? text.length : selectedRange.location))
            markedSelection = NSRange(location: location, length: min(selectedRange.length, text.length - location))
            keyProducedOutput = true
            releaseRemoteModifiers()
            needsDisplay = true
        }

        func unmarkText() {
            guard markedText.length > 0 else { return }
            let text = markedText.string
            clearMarkedText()
            sendText(text)
        }
        func hasMarkedText() -> Bool { markedText.length > 0 }
        func markedRange() -> NSRange {
            hasMarkedText() ? NSRange(location: 0, length: markedText.length) : NSRange(location: NSNotFound, length: 0)
        }
        func selectedRange() -> NSRange { markedSelection }
        func validAttributesForMarkedText() -> [NSAttributedString.Key] { [.underlineStyle, .foregroundColor] }
        func attributedSubstring(forProposedRange range: NSRange, actualRange: NSRangePointer?) -> NSAttributedString? {
            guard range.location != NSNotFound, range.location < markedText.length else {
                actualRange?.pointee = NSRange(location: NSNotFound, length: 0)
                return nil
            }
            let actual = NSIntersectionRange(range, NSRange(location: 0, length: markedText.length))
            actualRange?.pointee = actual
            return markedText.attributedSubstring(from: actual)
        }
        func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
            actualRange?.pointee = markedRange()
            let rect = CGRect(x: min(max(12, candidatePoint.x), max(12, bounds.width - 20)),
                              y: min(max(12, candidatePoint.y), max(12, bounds.height - 30)),
                              width: 1, height: 22)
            guard let window else { return rect }
            return window.convertToScreen(convert(rect, to: nil))
        }
        func characterIndex(for point: NSPoint) -> Int { NSNotFound }

        override func doCommand(by selector: Selector) {
            keyProducedOutput = true
            // Return, Delete, arrows and Escape are often candidate actions.
            // The input method owns both edges throughout that key event,
            // including a confirming key after it calls insertText.
            guard !keyBeganInComposition, !hasMarkedText(), !keyCommittedText,
                  let event = currentKeyEvent else {
                if selector == #selector(cancelOperation(_:)) { cancelComposition() }
                return
            }
            sendKeyboard(event, isDown: true)
        }

        override func draw(_ dirtyRect: NSRect) {
            super.draw(dirtyRect)
            guard hasMarkedText() else { return }
            let text = NSAttributedString(string: markedText.string, attributes: [
                .font: NSFont.systemFont(ofSize: 16), .foregroundColor: NSColor.labelColor,
                .underlineStyle: NSUnderlineStyle.single.rawValue,
            ])
            let size = text.size()
            let rect = CGRect(x: min(max(8, candidatePoint.x), max(8, bounds.width - size.width - 20)),
                              y: min(max(8, candidatePoint.y), max(8, bounds.height - size.height - 16)),
                              width: size.width + 16, height: size.height + 12)
            NSColor.controlBackgroundColor.withAlphaComponent(0.96).setFill()
            NSBezierPath(roundedRect: rect, xRadius: 6, yRadius: 6).fill()
            text.draw(at: CGPoint(x: rect.minX + 8, y: rect.minY + 6))
        }

        fileprivate func acceptsTextCallback(generation: UInt64) -> Bool {
            textInputEnabled && canControl && generation == textGeneration
        }

        private func sendPointer(_ event: NSEvent) {
            guard canControl, let point = normalizedPoint(for: event) else { return }
            sendInput(.move(normalizedX: Float(point.x), normalizedY: Float(point.y)))
        }

        private func sendButton(_ event: NSEvent, pressed: Bool) {
            guard canControl else { return }
            if pressed, !hasMarkedText() {
                setRemoteModifiers(KeyboardMapper.mappedModifiers(event.modifierFlags, mode: modifierMode))
            }
            let button: MouseButton = switch event.buttonNumber {
            case 0: .left
            case 1: .right
            default: .center
            }
            guard normalizedPoint(for: event) != nil else {
                if !pressed, pressedButtons.remove(button) != nil {
                    sendInput(.mouseButton(button, pressed: false))
                }
                return
            }
            sendPointer(event)
            sendInput(.mouseButton(button, pressed: pressed))
            if pressed { pressedButtons.insert(button) } else { pressedButtons.remove(button) }
        }

        private func normalizedPoint(for event: NSEvent) -> CGPoint? {
            viewport.normalizedPoint(for: convert(event.locationInWindow, from: nil))
        }
    }
}

/// AppKit may retain a text client while its input method finishes work. Give
/// each focus/stream lifetime a separate object so late replies are harmless.
@MainActor
private final class GenerationTextInputClient: NSObject, @preconcurrency NSTextInputClient {
    private weak var view: SessionInputView.InputView?
    private let generation: UInt64

    init(view: SessionInputView.InputView, generation: UInt64) {
        self.view = view
        self.generation = generation
    }

    private var activeView: SessionInputView.InputView? {
        guard let view, view.acceptsTextCallback(generation: generation) else { return nil }
        return view
    }

    func insertText(_ string: Any, replacementRange: NSRange) {
        activeView?.insertText(string, replacementRange: replacementRange)
    }
    func doCommand(by selector: Selector) { activeView?.doCommand(by: selector) }
    func setMarkedText(_ string: Any, selectedRange: NSRange, replacementRange: NSRange) {
        activeView?.setMarkedText(string, selectedRange: selectedRange, replacementRange: replacementRange)
    }
    func unmarkText() { activeView?.unmarkText() }
    func selectedRange() -> NSRange { activeView?.selectedRange() ?? NSRange(location: NSNotFound, length: 0) }
    func markedRange() -> NSRange { activeView?.markedRange() ?? NSRange(location: NSNotFound, length: 0) }
    func hasMarkedText() -> Bool { activeView?.hasMarkedText() ?? false }
    func attributedSubstring(forProposedRange range: NSRange, actualRange: NSRangePointer?) -> NSAttributedString? {
        guard let view = activeView else { actualRange?.pointee = NSRange(location: NSNotFound, length: 0); return nil }
        return view.attributedSubstring(forProposedRange: range, actualRange: actualRange)
    }
    func validAttributesForMarkedText() -> [NSAttributedString.Key] { activeView?.validAttributesForMarkedText() ?? [] }
    func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
        guard let view = activeView else { actualRange?.pointee = NSRange(location: NSNotFound, length: 0); return .zero }
        return view.firstRect(forCharacterRange: range, actualRange: actualRange)
    }
    func characterIndex(for point: NSPoint) -> Int { activeView?.characterIndex(for: point) ?? NSNotFound }
}

func quantizeScrollDelta(_ delta: CGFloat) -> Int32 {
    guard delta.isFinite, delta != 0 else { return 0 }
    let rounded = delta.rounded()
    let bounded = min(CGFloat(1_200), max(CGFloat(-1_200), rounded))
    if bounded == 0 { return delta < 0 ? -1 : 1 }
    return Int32(bounded)
}
