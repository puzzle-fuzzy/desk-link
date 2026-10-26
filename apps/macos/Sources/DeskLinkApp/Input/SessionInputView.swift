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
    let videoSize: CGSize?
    let visibleVideoRect: CGRect?
    let modifierMode: ModifierMode

    init(bridge: ControllerBridge, videoSize: CGSize? = nil, visibleVideoRect: CGRect? = nil, modifierMode: ModifierMode = .automatic) {
        self.bridge = bridge
        self.videoSize = videoSize
        self.visibleVideoRect = visibleVideoRect
        self.modifierMode = modifierMode
    }

    func makeNSView(context: Context) -> InputView {
        InputView(bridge: bridge, videoSize: videoSize, visibleVideoRect: visibleVideoRect, modifierMode: modifierMode)
    }

    func updateNSView(_ view: InputView, context: Context) {
        view.bridge = bridge
        view.videoSize = videoSize
        view.visibleVideoRect = visibleVideoRect
        view.synchronizeSession(bridge.state)
        view.synchronizeKeyboardPolicy(KeyboardMappingPolicy(mode: modifierMode, remotePlatform: bridge.remotePlatform))
        view.synchronizeInputContext(bridge.presentedInputContext)
    }

    static func dismantleNSView(_ view: InputView, coordinator: ()) {
        view.releaseAll()
    }

    final class InputView: NSView, NSTextInputClient {
        weak var bridge: ControllerBridge?
        var videoSize: CGSize? {
            didSet {
                needsDisplay = true
                inputContext?.invalidateCharacterCoordinates()
            }
        }
        var visibleVideoRect: CGRect? {
            didSet {
                needsDisplay = true
                inputContext?.invalidateCharacterCoordinates()
            }
        }
        private var pressedButtons: [MouseButton: CapturedInputContext] = [:]
        private var keyboard = KeyboardInputState()
        private var keyboardPolicy: KeyboardMappingPolicy
        private var sessionState: ConnectionState
        private let windowFocusObservation = WindowFocusObservation()
        private var markedText = NSMutableAttributedString(string: "")
        private var textSelection = NSRange(location: 0, length: 0)
        private var interpretingEvent: NSEvent?
        private var interpretingInputContext: CapturedInputContext?
        private var compositionInputContext: CapturedInputContext?
        private var textFocusContext: CapturedInputContext?
        private var surfaceContext: CapturedInputContext?
        private var compositionAtKeyDown = false
        private var composedDuringKeyDown = false
        private var discardingComposition = false
        // No remote caret geometry is negotiated yet. The last remote click
        // provides the known text-focus anchor, normalized to the video rect.
        private var textFocusAnchor = CGPoint(x: 0.5, y: 0.5)

        init(bridge: ControllerBridge, videoSize: CGSize?, visibleVideoRect: CGRect? = nil, modifierMode: ModifierMode = .automatic) {
            self.bridge = bridge
            self.videoSize = videoSize
            self.visibleVideoRect = visibleVideoRect
            keyboardPolicy = KeyboardMappingPolicy(mode: modifierMode, remotePlatform: bridge.remotePlatform)
            surfaceContext = bridge.presentedInputContext
            textFocusContext = bridge.captureInputContext()
            sessionState = bridge.state
            super.init(frame: .zero)
            wantsLayer = true
        }

        required init?(coder: NSCoder) { nil }

        override var acceptsFirstResponder: Bool { true }

        override func becomeFirstResponder() -> Bool {
            let accepted = super.becomeFirstResponder()
            if accepted { textFocusContext = bridge?.captureInputContext() }
            return accepted
        }

        override func draw(_ dirtyRect: NSRect) {
            super.draw(dirtyRect)
            guard hasMarkedText() else { return }
            let preview = NSMutableAttributedString(attributedString: markedText)
            preview.addAttributes(
                [.font: NSFont.systemFont(ofSize: 14), .foregroundColor: NSColor.textColor],
                range: NSRange(location: 0, length: preview.length)
            )
            let size = preview.size()
            let anchor = localTextAnchorRect()
            let origin = CGPoint(
                x: max(bounds.minX, min(anchor.minX, bounds.maxX - size.width - 8)),
                y: max(bounds.minY, min(anchor.minY, bounds.maxY - size.height - 4))
            )
            let background = NSRect(origin: origin, size: CGSize(width: size.width + 8, height: size.height + 4))
            NSColor.textBackgroundColor.setFill()
            NSBezierPath(roundedRect: background, xRadius: 3, yRadius: 3).fill()
            preview.draw(at: CGPoint(x: origin.x + 4, y: origin.y + 2))
        }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            windowFocusObservation.cancel()
            if window == nil {
                releaseAll()
                return
            }
            window?.acceptsMouseMovedEvents = true
            windowFocusObservation.token = NotificationCenter.default.addObserver(
                forName: NSWindow.didResignKeyNotification, object: window, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.releaseAll() }
            }
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                self.window?.makeFirstResponder(self)
            }
        }

        override func viewWillMove(toWindow newWindow: NSWindow?) {
            if newWindow == nil { releaseAll() }
            super.viewWillMove(toWindow: newWindow)
        }

        override func resignFirstResponder() -> Bool {
            releaseAll()
            return super.resignFirstResponder()
        }

        override func keyDown(with event: NSEvent) {
            guard let context = bridge?.captureInputContext() else { releaseAll(); return }
            synchronizeInputContext(context)
            textFocusContext = context
            if !hasMarkedText(), !event.modifierFlags.intersection([.command, .control]).isEmpty {
                sendKeyboard(event, isDown: true, context: context)
                return
            }
            interpretingEvent = event
            interpretingInputContext = context
            compositionAtKeyDown = hasMarkedText()
            composedDuringKeyDown = false
            // AppKit's input context owns pinyin, dead keys, candidate
            // selection and cancellation. Raw characters never bypass it.
            interpretKeyEvents([event])
            interpretingEvent = nil
            interpretingInputContext = nil
            compositionAtKeyDown = false
            composedDuringKeyDown = false
        }
        override func keyUp(with event: NSEvent) { sendKeyboard(event, isDown: false) }
        override func performKeyEquivalent(with event: NSEvent) -> Bool {
            guard window?.firstResponder === self, !hasMarkedText(), event.modifierFlags.contains(.command),
                  let context = bridge?.captureInputContext() else {
                return super.performKeyEquivalent(with: event)
            }
            // AppKit routes Command shortcuts through key equivalents before
            // keyDown. Send one balanced chord so local menus cannot consume it.
            synchronizeInputContext(context)
            sendKeyboard(event, isDown: true, context: context)
            sendKeyboard(event, isDown: false)
            return true
        }
        override func flagsChanged(with event: NSEvent) {
            guard !hasMarkedText() else { return }
            guard let context = bridge?.captureInputContext() else { releaseAll(); return }
            synchronizeInputContext(context)
            for command in keyboard.flagsChanged(context: context, event.modifierFlags, keyCode: event.keyCode, policy: keyboardPolicy) { bridge?.send(captured: command) }
        }
        override func mouseMoved(with event: NSEvent) { sendPointer(event) }
        override func mouseDragged(with event: NSEvent) { sendPointer(event, isDrag: true) }
        override func rightMouseDragged(with event: NSEvent) { sendPointer(event, isDrag: true) }
        override func otherMouseDragged(with event: NSEvent) { sendPointer(event, isDrag: true) }

        override func mouseDown(with event: NSEvent) { sendButton(event, pressed: true) }
        override func mouseUp(with event: NSEvent) { sendButton(event, pressed: false) }
        override func rightMouseDown(with event: NSEvent) { sendButton(event, pressed: true) }
        override func rightMouseUp(with event: NSEvent) { sendButton(event, pressed: false) }
        override func otherMouseDown(with event: NSEvent) { sendButton(event, pressed: true) }
        override func otherMouseUp(with event: NSEvent) { sendButton(event, pressed: false) }

        override func scrollWheel(with event: NSEvent) {
            let deltaX = quantizeScrollDelta(event.scrollingDeltaX)
            let deltaY = quantizeScrollDelta(event.scrollingDeltaY)
            guard deltaX != 0 || deltaY != 0 else { return }
            guard let context = bridge?.captureInputContext() else { return }
            synchronizeInputContext(context)
            bridge?.send(input: .wheel(deltaX: deltaX, deltaY: deltaY), context: context)
        }

        func releaseAll() {
            let textContexts = [compositionInputContext, interpretingInputContext, textFocusContext].compactMap { $0 }
            let contexts = keyboard.activeContexts.union(pressedButtons.values).union(textContexts)
            for context in contexts { bridge?.releaseAll(context: context) }
            pressedButtons.removeAll()
            keyboard.reset()
            discardComposition()
            textFocusContext = nil
            interpretingInputContext = nil
        }

        private func releasePhysicalInputs() {
            let contexts = keyboard.activeContexts.union(pressedButtons.values)
            for context in contexts { bridge?.releaseAll(context: context) }
            pressedButtons.removeAll()
            keyboard.reset()
        }

        func synchronizeInputContext(_ context: CapturedInputContext?) {
            guard context?.targetIdentity != surfaceContext?.targetIdentity else { return }
            releaseAll()
            surfaceContext = context
        }

        func synchronizeSession(_ state: ConnectionState) {
            guard state != sessionState else { return }
            sessionState = state
            switch state {
            case .connected, .recovering, .frozen: break
            default: releaseAll()
            }
        }

        func synchronizeKeyboardPolicy(_ policy: KeyboardMappingPolicy) {
            guard policy != keyboardPolicy else { return }
            releaseAll()
            keyboardPolicy = policy
        }

        func hasMarkedText() -> Bool { markedText.length > 0 }

        func markedRange() -> NSRange {
            hasMarkedText() ? NSRange(location: 0, length: markedText.length) : NSRange(location: NSNotFound, length: 0)
        }

        func selectedRange() -> NSRange { textSelection }

        func validAttributesForMarkedText() -> [NSAttributedString.Key] { [.underlineStyle, .markedClauseSegment] }

        func setMarkedText(_ value: Any, selectedRange: NSRange, replacementRange: NSRange) {
            guard let context = compositionInputContext ?? interpretingInputContext ?? textFocusContext else { return }
            if !hasMarkedText() {
                releasePhysicalInputs()
            }
            compositionInputContext = context
            composedDuringKeyDown = true
            let text = attributedText(value)
            let target: NSRange
            if replacementRange.location != NSNotFound,
               replacementRange.location <= markedText.length,
               replacementRange.length <= markedText.length - replacementRange.location {
                target = replacementRange
            } else {
                target = NSRange(location: 0, length: markedText.length)
            }
            markedText.replaceCharacters(in: target, with: text)
            let location = min(selectedRange.location, markedText.length)
            textSelection = NSRange(location: location, length: min(selectedRange.length, markedText.length - location))
            needsDisplay = true
            inputContext?.invalidateCharacterCoordinates()
        }

        func unmarkText() {
            let committed = markedText.string
            let context = compositionInputContext
            clearComposition()
            if !discardingComposition, !committed.isEmpty, let context {
                bridge?.send(input: .unicode(committed, modifiers: []), context: context)
            }
        }

        func insertText(_ value: Any, replacementRange: NSRange) {
            let text = attributedText(value).string
            let wasComposing = hasMarkedText() || compositionAtKeyDown || composedDuringKeyDown
            let context = compositionInputContext ?? interpretingInputContext ?? textFocusContext
            clearComposition()
            guard !discardingComposition, !text.isEmpty, let context else { return }
            // Preserve physical press/repeat/release for direct typing. IME
            // and dead-key commits have atomic text semantics instead.
            if !wasComposing, let event = interpretingEvent,
               text == event.characters,
               !event.modifierFlags.contains(.option),
               text.unicodeScalars.count == 1,
               text.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) }) {
                sendKeyboard(event, isDown: true, context: context)
            } else {
                bridge?.send(input: .unicode(text, modifiers: []), context: context)
            }
        }

        override func doCommand(by selector: Selector) {
            if hasMarkedText() || compositionAtKeyDown || composedDuringKeyDown {
                if selector == #selector(NSResponder.cancelOperation(_:)) { discardComposition() }
                return
            }
            guard let event = interpretingEvent, let context = interpretingInputContext,
                  MacKeyCodeMapper.protocolCode(forAppKitKeyCode: event.keyCode) != nil else { return }
            sendKeyboard(event, isDown: true, context: context)
        }

        func attributedSubstring(forProposedRange range: NSRange, actualRange: NSRangePointer?) -> NSAttributedString? {
            guard range.location != NSNotFound, range.location <= markedText.length else {
                actualRange?.pointee = NSRange(location: NSNotFound, length: 0)
                return nil
            }
            let available = NSRange(location: range.location, length: min(range.length, markedText.length - range.location))
            actualRange?.pointee = available
            return markedText.attributedSubstring(from: available)
        }

        func characterIndex(for point: NSPoint) -> Int { textSelection.location }

        func firstRect(forCharacterRange range: NSRange, actualRange: NSRangePointer?) -> NSRect {
            actualRange?.pointee = selectedRange()
            let anchor = localTextAnchorRect()
            guard let window else { return .zero }
            return window.convertToScreen(convert(anchor, to: nil))
        }

        private func localTextAnchorRect() -> NSRect {
            let videoRect = inputVideoRect
            return NSRect(
                x: videoRect.minX + textFocusAnchor.x * videoRect.width,
                y: videoRect.maxY - textFocusAnchor.y * videoRect.height,
                width: 0, height: 18
            )
        }

        private func attributedText(_ value: Any) -> NSAttributedString {
            if let text = value as? NSAttributedString { return text }
            return NSAttributedString(string: value as? String ?? "")
        }

        private func clearComposition() {
            markedText = NSMutableAttributedString(string: "")
            compositionInputContext = nil
            textSelection = NSRange(location: 0, length: 0)
            needsDisplay = true
            inputContext?.invalidateCharacterCoordinates()
        }

        private func discardComposition() {
            discardingComposition = true
            inputContext?.discardMarkedText()
            clearComposition()
            discardingComposition = false
        }

        private func sendKeyboard(_ event: NSEvent, isDown: Bool, context: CapturedInputContext? = nil) {
            let commands: [CapturedRemoteInput]
            if isDown {
                guard let context else { return }
                let isShortcut = !event.modifierFlags.intersection([.command, .control]).isEmpty
                commands = keyboard.keyDown(
                    context: context,
                    keyCode: event.keyCode,
                    characters: isShortcut ? event.charactersIgnoringModifiers : event.characters,
                    modifiers: event.modifierFlags,
                    isRepeat: event.isARepeat,
                    policy: keyboardPolicy
                )
            } else {
                commands = keyboard.keyUp(keyCode: event.keyCode)
            }
            for command in commands {
                bridge?.send(captured: command)
            }
        }

        private func sendPointer(_ event: NSEvent, isDrag: Bool = false) {
            let context = isDrag ? pressedButtons.values.first : bridge?.captureInputContext()
            guard let context else { return }
            if !isDrag { synchronizeInputContext(context) }
            sendPointer(event, context: context)
        }

        private func sendPointer(_ event: NSEvent, context: CapturedInputContext) {
            guard let normalizedPoint = normalizedPoint(for: event) else { return }
            bridge?.send(input: .move(
                normalizedX: Float(normalizedPoint.x),
                normalizedY: Float(normalizedPoint.y)
            ), context: context)
        }

        private func sendButton(_ event: NSEvent, pressed: Bool) {
            let button: MouseButton
            switch event.buttonNumber {
            case 0: button = .left
            case 1: button = .right
            default: button = .center
            }
            if !pressed {
                guard let context = pressedButtons.removeValue(forKey: button) else { return }
                if bridge?.captureInputContext()?.hasSameInputTarget(as: context) == true { sendPointer(event, context: context) }
                bridge?.send(input: .mouseButton(button, pressed: false), context: context)
                return
            }
            guard let context = bridge?.captureInputContext() else { return }
            synchronizeInputContext(context)
            guard let point = normalizedPoint(for: event) else {
                return
            }
            if pressed {
                textFocusAnchor = point
                needsDisplay = true
                inputContext?.invalidateCharacterCoordinates()
            }
            sendPointer(event, context: context)
            bridge?.send(input: .mouseButton(button, pressed: true), context: context)
            pressedButtons[button] = context
        }

        private func normalizedPoint(for event: NSEvent) -> CGPoint? {
            InputMapper(videoRect: inputVideoRect).normalizedPoint(for: convert(event.locationInWindow, from: nil))
        }

        private var inputVideoRect: CGRect {
            // A production rect is supplied only after drawable presentation.
            // An explicitly empty rect means input is not ready.
            visibleVideoRect ?? VideoGeometry.aspectFit(source: videoSize ?? bounds.size, in: bounds)
        }
    }
}

func quantizeScrollDelta(_ delta: CGFloat) -> Int32 {
    guard delta.isFinite, delta != 0 else { return 0 }
    let rounded = delta.rounded()
    let bounded = min(CGFloat(1_200), max(CGFloat(-1_200), rounded))
    if bounded == 0 { return delta < 0 ? -1 : 1 }
    return Int32(bounded)
}
