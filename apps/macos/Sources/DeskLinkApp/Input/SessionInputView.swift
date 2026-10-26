import AppKit
import DeskLinkAppleCore
import SwiftUI

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

    final class InputView: NSView {
        weak var bridge: ControllerBridge?
        var videoSize: CGSize?
        private var pressedButtons = Set<MouseButton>()
        private var keyboard = KeyboardInputState()
        private var sessionState: ConnectionState
        private var windowFocusObserver: NSObjectProtocol?

        init(bridge: ControllerBridge, videoSize: CGSize?) {
            self.bridge = bridge
            self.videoSize = videoSize
            sessionState = bridge.state
            super.init(frame: .zero)
            wantsLayer = true
        }

        required init?(coder: NSCoder) { nil }

        override var acceptsFirstResponder: Bool { true }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if let windowFocusObserver { NotificationCenter.default.removeObserver(windowFocusObserver) }
            windowFocusObserver = nil
            if window == nil {
                releaseAll()
                return
            }
            window?.acceptsMouseMovedEvents = true
            windowFocusObserver = NotificationCenter.default.addObserver(
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

        override func keyDown(with event: NSEvent) { sendKeyboard(event, isDown: true) }
        override func keyUp(with event: NSEvent) { sendKeyboard(event, isDown: false) }
        override func flagsChanged(with event: NSEvent) {
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
            if let windowFocusObserver { NotificationCenter.default.removeObserver(windowFocusObserver) }
            let bridge = bridge
            Task { @MainActor in bridge?.releaseAll() }
        }

        func releaseAll() {
            bridge?.releaseAll()
            pressedButtons.removeAll()
            keyboard.reset()
        }

        func synchronizeSession(_ state: ConnectionState) {
            guard state != sessionState else { return }
            releaseAll()
            sessionState = state
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
            guard normalizedPoint(for: event) != nil else {
                if !pressed, pressedButtons.remove(button) != nil {
                    bridge?.send(input: .mouseButton(button, pressed: false))
                }
                return
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
