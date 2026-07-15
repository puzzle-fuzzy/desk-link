import AppKit
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
    }

    static func dismantleNSView(_ view: InputView, coordinator: ()) {
        view.releaseAll()
    }

    final class InputView: NSView {
        weak var bridge: ControllerBridge?
        var videoSize: CGSize?
        private var pressedButtons = Set<MouseButton>()

        init(bridge: ControllerBridge, videoSize: CGSize?) {
            self.bridge = bridge
            self.videoSize = videoSize
            super.init(frame: .zero)
            wantsLayer = true
        }

        required init?(coder: NSCoder) { nil }

        override var acceptsFirstResponder: Bool { true }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if window == nil {
                releaseAll()
                return
            }
            window?.acceptsMouseMovedEvents = true
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
            bridge?.send(input: .wheel(
                deltaX: Int32(event.scrollingDeltaX.rounded()),
                deltaY: Int32(event.scrollingDeltaY.rounded())
            ))
        }

        deinit {
            let bridge = bridge
            Task { @MainActor in bridge?.releaseAll() }
        }

        func releaseAll() {
            bridge?.releaseAll()
            pressedButtons.removeAll()
        }

        private func sendKeyboard(_ event: NSEvent, isDown: Bool) {
            for command in KeyboardMapper.map(
                keyCode: event.keyCode,
                characters: event.characters,
                modifiers: event.modifierFlags,
                isDown: isDown
            ) {
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
