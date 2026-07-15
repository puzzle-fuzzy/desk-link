import CoreGraphics

enum ModifierMode: Equatable {
    case automatic
    case raw
}

enum LocalModifier: Equatable {
    case shift
    case command
    case option
    case control
    case capsLock
}

enum RemoteModifier: Equatable {
    case shift
    case control
    case alt
    case command
    case capsLock
}

struct InputMapper {
    let videoRect: CGRect
    let modifierMode: ModifierMode

    init(videoRect: CGRect, modifierMode: ModifierMode = .automatic) {
        self.videoRect = videoRect
        self.modifierMode = modifierMode
    }

    func normalizedPoint(for point: CGPoint) -> CGPoint? {
        guard videoRect.width > 0,
              videoRect.height > 0,
              point.x >= videoRect.minX,
              point.x <= videoRect.maxX,
              point.y >= videoRect.minY,
              point.y <= videoRect.maxY
        else {
            return nil
        }
        return CGPoint(
            x: (point.x - videoRect.minX) / videoRect.width,
            // AppKit's default NSView coordinate system is bottom-origin, while the
            // protocol and Quartz display coordinates use a top-origin convention.
            y: 1 - (point.y - videoRect.minY) / videoRect.height
        )
    }

    func remoteModifier(for modifier: LocalModifier) -> RemoteModifier {
        guard modifierMode == .automatic else {
            return rawModifier(modifier)
        }
        switch modifier {
        case .command: return .control
        case .option: return .alt
        default: return rawModifier(modifier)
        }
    }

    private func rawModifier(_ modifier: LocalModifier) -> RemoteModifier {
        switch modifier {
        case .shift: .shift
        case .command: .command
        case .option: .alt
        case .control: .control
        case .capsLock: .capsLock
        }
    }
}
