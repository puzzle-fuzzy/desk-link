import CoreGraphics
import DeskLinkAppleCore

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
        // AppKit's default NSView coordinate system is bottom-origin. Convert
        // it once at the platform boundary, then use the shared top-left
        // protocol convention everywhere else.
        let topLeftPoint = CGPoint(
            x: point.x,
            y: videoRect.maxY - (point.y - videoRect.minY)
        )
        return VideoGeometry.normalizedTopLeftPoint(for: topLeftPoint, in: videoRect)
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
