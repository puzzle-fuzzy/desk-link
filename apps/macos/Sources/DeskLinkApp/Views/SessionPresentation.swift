import AppKit
import Combine
import SwiftUI

enum SessionFullscreenPhase: Equatable {
    case windowed
    case entering
    case fullscreen
    case exiting
}

@MainActor
final class SessionPresentation: NSObject, ObservableObject {
    @Published var scalingMode: SessionScalingMode = .fit
    @Published var interactionMode: SessionInteractionMode = .control
    @Published var panOffset: CGSize = .zero
    @Published private(set) var backingScale: CGFloat = 1
    @Published private(set) var isFullscreen = false
    @Published private(set) var toolbarRevealed = true
    @Published var toolbarHovered = false
    var releaseInput: (() -> Void)?
    private weak var window: NSWindow?
    private var hideTask: Task<Void, Never>?
    private(set) var fullscreenPhase: SessionFullscreenPhase = .windowed
    private(set) var cleanupRequested = false
    private var exitAfterEnter = false
    private let toggleWindowFullscreen: (NSWindow) -> Void
    // SwiftUI can release its StateObject while the native window is still
    // animating. Keep the notification observer alive until exit or close.
    private var deferredCleanupOwner: SessionPresentation?

    init(toggleWindowFullscreen: @escaping (NSWindow) -> Void = { $0.toggleFullScreen(nil) }) {
        self.toggleWindowFullscreen = toggleWindowFullscreen
        super.init()
    }

    var toolbarIsVisible: Bool { !isFullscreen || toolbarHovered || toolbarRevealed }

    func attach(window: NSWindow?) {
        guard self.window !== window else { return }
        if window == nil, fullscreenPhase != .windowed {
            cleanup()
            return
        }
        NotificationCenter.default.removeObserver(self)
        self.window = window
        guard let window else { return }
        cleanupRequested = false
        exitAfterEnter = false
        isFullscreen = window.styleMask.contains(.fullScreen)
        fullscreenPhase = isFullscreen ? .fullscreen : .windowed
        for name in [NSWindow.didEnterFullScreenNotification, NSWindow.didExitFullScreenNotification,
                     NSWindow.didChangeBackingPropertiesNotification] {
            NotificationCenter.default.addObserver(self, selector: #selector(windowChanged), name: name, object: window)
        }
        for name in [NSWindow.willEnterFullScreenNotification, NSWindow.willExitFullScreenNotification,
                     NSWindow.didResignKeyNotification] {
            NotificationCenter.default.addObserver(self, selector: #selector(releaseForWindowTransition), name: name, object: window)
        }
        NotificationCenter.default.addObserver(self, selector: #selector(windowWillClose),
                                               name: NSWindow.willCloseNotification, object: window)
        synchronizeWindow()
    }

    func setScalingMode(_ mode: SessionScalingMode) {
        guard mode != scalingMode else { return }
        releaseInput?()
        panOffset = .zero
        scalingMode = mode
        revealToolbar()
    }

    func setInteractionMode(_ mode: SessionInteractionMode) {
        guard mode != interactionMode else { return }
        releaseInput?()
        interactionMode = mode
        revealToolbar()
    }

    func toggleFullscreen() {
        guard !cleanupRequested, let window,
              fullscreenPhase == .windowed || fullscreenPhase == .fullscreen else { return }
        releaseInput?()
        fullscreenPhase = isFullscreen ? .exiting : .entering
        toggleWindowFullscreen(window)
    }

    @discardableResult
    func exitFullscreen() -> Bool {
        switch fullscreenPhase {
        case .windowed:
            return false
        case .entering:
            exitAfterEnter = true
            releaseInput?()
            return true
        case .exiting:
            return true
        case .fullscreen:
            releaseInput?()
            fullscreenPhase = .exiting
            if let window { toggleWindowFullscreen(window) }
            return true
        }
    }

    func revealToolbar() {
        toolbarRevealed = true
        hideTask?.cancel()
        guard isFullscreen, !cleanupRequested else { return }
        hideTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(3))
            guard !Task.isCancelled else { return }
            self?.toolbarRevealed = false
        }
    }

    func cleanup() {
        guard !cleanupRequested else { return }
        cleanupRequested = true
        hideTask?.cancel()
        releaseInput?()
        guard fullscreenPhase != .windowed, window != nil else {
            finishCleanup()
            return
        }
        deferredCleanupOwner = self
        _ = exitFullscreen()
    }

    private func finishCleanup() {
        NotificationCenter.default.removeObserver(self)
        window = nil
        releaseInput = nil
        deferredCleanupOwner = nil
    }

    @objc private func windowChanged(_ notification: Notification) {
        switch notification.name {
        case NSWindow.didEnterFullScreenNotification:
            fullscreenPhase = .fullscreen
            isFullscreen = true
            if cleanupRequested || exitAfterEnter { _ = exitFullscreen() }
        case NSWindow.didExitFullScreenNotification:
            fullscreenPhase = .windowed
            isFullscreen = false
            exitAfterEnter = false
            if cleanupRequested {
                finishCleanup()
                return
            }
        default:
            break
        }
        synchronizeWindow()
    }

    @objc private func releaseForWindowTransition(_ notification: Notification) {
        if notification.name == NSWindow.willEnterFullScreenNotification { fullscreenPhase = .entering }
        if notification.name == NSWindow.willExitFullScreenNotification { fullscreenPhase = .exiting }
        releaseInput?()
    }

    @objc private func windowWillClose(_ notification: Notification) {
        hideTask?.cancel()
        releaseInput?()
        finishCleanup()
    }

    private func synchronizeWindow() {
        backingScale = window?.backingScaleFactor ?? 1
        revealToolbar()
    }
}

struct SessionWindowReader: NSViewRepresentable {
    let onWindow: (NSWindow?) -> Void

    func makeNSView(context: Context) -> WindowView {
        let view = WindowView()
        view.onWindow = onWindow
        return view
    }

    func updateNSView(_ view: WindowView, context: Context) { view.onWindow = onWindow }

    final class WindowView: NSView {
        var onWindow: ((NSWindow?) -> Void)?
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                self.onWindow?(self.window)
            }
        }
    }
}
