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

    init(bridge: ControllerBridge, videoSize: CGSize? = nil) {
        self.bridge = bridge
        self.videoSize = videoSize
    }

    func makeNSView(context: Context) -> InputView {
        InputView(bridge: bridge, videoSize: videoSize)
    }

    func updateNSView(_ view: InputView, context: Context) {
        view.bridge = bridge
        view.videoSize = videoSize
        view.synchronizeSession(bridge.state)
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
        private var pressedButtons = Set<MouseButton>()
        private var keyboard = KeyboardInputState()
        private var sessionState: ConnectionState
        private let windowFocusObservation = WindowFocusObservation()
        private var markedText = NSMutableAttributedString(string: "")
        private var textSelection = NSRange(location: 0, length: 0)
        private var interpretingEvent: NSEvent?
        private var compositionAtKeyDown = false
        private var composedDuringKeyDown = false
        private var discardingComposition = false
        // No remote caret geometry is negotiated yet. The last remote click
        // provides the known text-focus anchor, normalized to the video rect.
        private var textFocusAnchor = CGPoint(x: 0.5, y: 0.5)

        init(bridge: ControllerBridge, videoSize: CGSize?) {
            self.bridge = bridge
            self.videoSize = videoSize
            sessionState = bridge.state
            super.init(frame: .zero)
            wantsLayer = true
        }

        required init?(coder: NSCoder) { nil }

        override var acceptsFirstResponder: Bool { true }

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
                Task { @MainActor in self?.releaseAll() }
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
            if !hasMarkedText(), !event.modifierFlags.intersection([.command, .control]).isEmpty {
                sendKeyboard(event, isDown: true)
                return
            }
            interpretingEvent = event
            compositionAtKeyDown = hasMarkedText()
            composedDuringKeyDown = false
            // AppKit's input context owns pinyin, dead keys, candidate
            // selection and cancellation. Raw characters never bypass it.
            interpretKeyEvents([event])
            interpretingEvent = nil
            compositionAtKeyDown = false
            composedDuringKeyDown = false
        }
        override func keyUp(with event: NSEvent) { sendKeyboard(event, isDown: false) }
        override func flagsChanged(with event: NSEvent) {
            guard !hasMarkedText() else { return }
            for command in keyboard.flagsChanged(event.modifierFlags) { bridge?.send(input: command) }
        }
        override func mouseMoved(with event: NSEvent) { sendPointer(event) }
        override func mouseDragged(with event: NSEvent) { sendPointer(event) }
        override func rightMouseDragged(with event: NSEvent) { sendPointer(event) }
        override func otherMouseDragged(with event: NSEvent) { sendPointer(event) }

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
            bridge?.send(input: .wheel(deltaX: deltaX, deltaY: deltaY))
        }

        deinit {
            let bridge = bridge
            Task { @MainActor in bridge?.releaseAll() }
        }

        func releaseAll() {
            discardComposition()
            bridge?.releaseAll()
            pressedButtons.removeAll()
            keyboard.reset()
        }

        func synchronizeSession(_ state: ConnectionState) {
            guard state != sessionState else { return }
            releaseAll()
            sessionState = state
        }

        func hasMarkedText() -> Bool { markedText.length > 0 }

        func markedRange() -> NSRange {
            hasMarkedText() ? NSRange(location: 0, length: markedText.length) : NSRange(location: NSNotFound, length: 0)
        }

        func selectedRange() -> NSRange { textSelection }

        func validAttributesForMarkedText() -> [NSAttributedString.Key] { [.underlineStyle, .markedClauseSegment] }

        func setMarkedText(_ value: Any, selectedRange: NSRange, replacementRange: NSRange) {
            if !hasMarkedText() {
                bridge?.releaseAll()
                pressedButtons.removeAll()
                keyboard.reset()
            }
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
            clearComposition()
            if !discardingComposition, !committed.isEmpty { bridge?.send(input: .unicode(committed, modifiers: [])) }
        }

        func insertText(_ value: Any, replacementRange: NSRange) {
            let text = attributedText(value).string
            let wasComposing = hasMarkedText() || compositionAtKeyDown || composedDuringKeyDown
            clearComposition()
            guard !discardingComposition, !text.isEmpty else { return }
            // Preserve physical press/repeat/release for direct typing. IME
            // and dead-key commits have atomic text semantics instead.
            if !wasComposing, let event = interpretingEvent,
               text == event.characters,
               !event.modifierFlags.contains(.option),
               text.unicodeScalars.count == 1,
               text.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) }) {
                sendKeyboard(event, isDown: true)
            } else {
                bridge?.send(input: .unicode(text, modifiers: []))
            }
        }

        override func doCommand(by selector: Selector) {
            if hasMarkedText() || compositionAtKeyDown || composedDuringKeyDown {
                if selector == #selector(NSResponder.cancelOperation(_:)) { discardComposition() }
                return
            }
            guard let event = interpretingEvent,
                  MacKeyCodeMapper.protocolCode(forAppKitKeyCode: event.keyCode) != nil else { return }
            sendKeyboard(event, isDown: true)
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
            let videoRect = VideoGeometry.aspectFit(source: videoSize ?? bounds.size, in: bounds)
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

        private func sendKeyboard(_ event: NSEvent, isDown: Bool) {
            let commands: [RemoteInputCommand]
            if isDown {
                let isShortcut = !event.modifierFlags.intersection([.command, .control]).isEmpty
                commands = keyboard.keyDown(
                    keyCode: event.keyCode,
                    characters: isShortcut ? event.charactersIgnoringModifiers : event.characters,
                    modifiers: event.modifierFlags,
                    isRepeat: event.isARepeat
                )
            } else {
                commands = keyboard.keyUp(keyCode: event.keyCode)
            }
            for command in commands {
                bridge?.send(input: command)
            }
        }

        private func sendPointer(_ event: NSEvent) {
            guard let normalizedPoint = normalizedPoint(for: event) else { return }
            bridge?.send(input: .move(
                normalizedX: Float(normalizedPoint.x),
                normalizedY: Float(normalizedPoint.y)
            ))
        }

        private func sendButton(_ event: NSEvent, pressed: Bool) {
            let button: MouseButton
            switch event.buttonNumber {
            case 0: button = .left
            case 1: button = .right
            default: button = .center
            }
            guard let point = normalizedPoint(for: event) else {
                if !pressed, pressedButtons.remove(button) != nil {
                    bridge?.send(input: .mouseButton(button, pressed: false))
                }
                return
            }
            if pressed {
                textFocusAnchor = point
                needsDisplay = true
                inputContext?.invalidateCharacterCoordinates()
            }
            sendPointer(event)
            bridge?.send(input: .mouseButton(button, pressed: pressed))
            if pressed { pressedButtons.insert(button) } else { pressedButtons.remove(button) }
        }

        private func normalizedPoint(for event: NSEvent) -> CGPoint? {
            let source = videoSize ?? bounds.size
            let videoRect = VideoGeometry.aspectFit(source: source, in: bounds)
            return InputMapper(videoRect: videoRect).normalizedPoint(for: convert(event.locationInWindow, from: nil))
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
