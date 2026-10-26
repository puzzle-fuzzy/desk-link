import CoreGraphics
import Foundation

/// Identity of a frame that the local presentation surface actually displayed.
public struct CapturedInputContext: Equatable, Hashable, Sendable {
    public struct TargetIdentity: Equatable, Hashable, Sendable {
        public let streamID: UInt64
        public let inputEpoch: UInt64
        public let runtimeGeneration: UInt64
    }

    public let streamID: UInt64
    public let inputEpoch: UInt64
    public let configVersion: UInt32
    public let runtimeGeneration: UInt64

    /// Quality-only reconfiguration changes configVersion without changing
    /// the remote input target. The full context still keeps its original value.
    public var targetIdentity: TargetIdentity {
        TargetIdentity(streamID: streamID, inputEpoch: inputEpoch, runtimeGeneration: runtimeGeneration)
    }

    public func hasSameInputTarget(as other: CapturedInputContext) -> Bool {
        targetIdentity == other.targetIdentity
    }

    public init(streamID: UInt64, inputEpoch: UInt64, configVersion: UInt32, runtimeGeneration: UInt64) {
        self.streamID = streamID
        self.inputEpoch = inputEpoch
        self.configVersion = configVersion
        self.runtimeGeneration = runtimeGeneration
    }
}

public struct CapturedRemoteInput: Equatable, Sendable {
    public let command: RemoteInputCommand
    public let context: CapturedInputContext

    public init(_ command: RemoteInputCommand, context: CapturedInputContext) {
        self.command = command
        self.context = context
    }
}
