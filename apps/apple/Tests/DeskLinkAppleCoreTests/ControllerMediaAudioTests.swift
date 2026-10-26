import Foundation
import XCTest
@testable import DeskLinkAppleCore

@MainActor
final class ControllerMediaAudioTests: XCTestCase {
    func testAudioEnablesOnlyAfterRequestedMatchingHostAcknowledgement() throws {
        let media = connectedMedia()
        var actions: [ControllerMediaAction] = []
        media.performAction = { actions.append($0); return true }
        var received: [RemoteAudioFrame] = []
        media.onAudioFrame = { received.append($0) }
        try acceptState(media, available: true, enabled: false)
        XCTAssertEqual(media.audioAvailable, true)
        XCTAssertTrue(media.setAudioEnabled(true))
        XCTAssertEqual(actions, [.setAudio(streamID: 42, enabled: true)])
        XCTAssertTrue(media.audioPending)
        XCTAssertFalse(media.audioEnabled)
        acceptPCM(media, sequence: 1)
        try acceptState(media, available: true, enabled: false)
        XCTAssertTrue(media.audioPending, "an opposite state is not the enable acknowledgement")
        XCTAssertTrue(received.isEmpty)
        try acceptState(media, available: true, enabled: true)
        XCTAssertTrue(media.audioEnabled)
        XCTAssertFalse(media.audioPending)
        XCTAssertNil(media.audioError)
        acceptPCM(media, sequence: 2)
        XCTAssertEqual(received.count, 1)
        XCTAssertEqual(received.first?.streamID, 42)
        XCTAssertEqual(received.first?.sequence, 2)
        XCTAssertEqual(received.first?.pcm.count, 960)
        media.reset()
    }

    func testUnrequestedEnabledStateNeverStartsLocalPlayback() throws {
        let media = connectedMedia()
        var frames = 0
        var starts = 0
        media.onAudioFrame = { _ in frames += 1 }
        media.onAudioStateChanged = { _, enabled in if enabled { starts += 1 } }
        try acceptState(media, available: true, enabled: true)
        acceptPCM(media, sequence: 1)
        XCTAssertEqual(media.audioAvailable, true)
        XCTAssertFalse(media.audioEnabled)
        XCTAssertFalse(media.audioPending)
        XCTAssertEqual(frames, 0)
        XCTAssertEqual(starts, 0)
    }

    func testRealEnabledStateRestoresExistingConsentAfterOlderMutedAcknowledgement() throws {
        let media = connectedMedia()
        var states: [Bool] = []
        var frames: [UInt64] = []
        media.onAudioStateChanged = { _, enabled in states.append(enabled) }
        media.onAudioFrame = { frames.append($0.sequence) }
        XCTAssertTrue(media.setAudioEnabled(true))
        try acceptState(media, available: true, enabled: true) // cached/early enabled state
        acceptPCM(media, sequence: 10)
        XCTAssertTrue(media.audioEnabled)
        XCTAssertFalse(media.audioPending)
        try acceptState(media, available: true, enabled: false) // an older initial mute response
        acceptPCM(media, sequence: 11)
        XCTAssertFalse(media.audioEnabled)
        XCTAssertEqual(frames, [10])
        try acceptState(media, available: true, enabled: true) // actual current host state
        acceptPCM(media, sequence: 1)
        XCTAssertTrue(media.audioEnabled)
        XCTAssertFalse(media.audioPending)
        XCTAssertEqual(states, [true, false, true])
        XCTAssertEqual(frames, [10, 1], "the muted transition resets audio sequence ordering")
        XCTAssertTrue(media.setAudioEnabled(false))
        try acceptState(media, available: true, enabled: false)
        try acceptState(media, available: true, enabled: true)
        acceptPCM(media, sequence: 2)
        XCTAssertFalse(media.audioEnabled, "explicit off revokes consent despite later enabled states")
        XCTAssertEqual(frames, [10, 1])
        media.reset()
    }

    func testOffStopsBeforeHostAcknowledgementAndRejectsLateEnabledState() throws {
        let media = connectedMedia()
        try enable(media)
        var states: [Bool] = []
        var frames = 0
        media.onAudioStateChanged = { _, enabled in states.append(enabled) }
        media.onAudioFrame = { _ in frames += 1 }
        XCTAssertTrue(media.setAudioEnabled(false))
        XCTAssertFalse(media.audioEnabled)
        XCTAssertTrue(media.audioPending)
        XCTAssertEqual(states, [false], "local playback stops synchronously on the off action")
        try acceptState(media, available: true, enabled: true)
        acceptPCM(media, sequence: 100)
        XCTAssertFalse(media.audioEnabled)
        XCTAssertTrue(media.audioPending)
        XCTAssertEqual(frames, 0)
        try acceptState(media, available: true, enabled: false)
        XCTAssertFalse(media.audioPending)
        XCTAssertFalse(media.audioEnabled)
        XCTAssertEqual(states, [false, false])
        media.reset()
    }

    func testOffCancelsPendingEnableAndCannotBeUndoneByItsLateAcknowledgement() throws {
        let media = connectedMedia()
        var actions: [ControllerMediaAction] = []
        var states: [Bool] = []
        media.performAction = { actions.append($0); return true }
        media.onAudioStateChanged = { _, enabled in states.append(enabled) }
        XCTAssertTrue(media.setAudioEnabled(true))
        XCTAssertTrue(media.audioPending)
        XCTAssertTrue(media.setAudioEnabled(false), "off must cancel a pending enable immediately")
        XCTAssertEqual(actions, [.setAudio(streamID: 42, enabled: true), .setAudio(streamID: 42, enabled: false)])
        XCTAssertEqual(states, [false])
        try acceptState(media, available: true, enabled: true)
        XCTAssertFalse(media.audioEnabled)
        XCTAssertTrue(media.audioPending)
        try acceptState(media, available: true, enabled: false)
        XCTAssertFalse(media.audioEnabled)
        XCTAssertFalse(media.audioPending)
        media.reset()
    }

    func testOldStreamAndReconnectPacketsCannotStartPlayback() throws {
        let media = connectedMedia()
        try enable(media)
        var frames: [UInt64] = []
        media.onAudioFrame = { frames.append($0.sequence) }
        let oldContext = media.contextID
        media.reset()
        XCTAssertGreaterThan(media.contextID, oldContext)
        media.attach(streamID: 43)
        try acceptState(media, streamID: 42, available: true, enabled: true)
        acceptPCM(media, streamID: 42, sequence: 100)
        acceptPCM(media, streamID: 43, sequence: 1)
        XCTAssertFalse(media.audioEnabled)
        XCTAssertTrue(frames.isEmpty)
        try enable(media, streamID: 43)
        acceptPCM(media, streamID: 42, sequence: 101)
        acceptPCM(media, streamID: 43, sequence: 1)
        XCTAssertEqual(frames, [1])
        media.reset()
        media.attach(streamID: 43) // a restarted peer can legitimately reuse its stream ID
        try acceptState(media, streamID: 43, available: true, enabled: true)
        acceptPCM(media, streamID: 43, sequence: 2)
        XCTAssertFalse(media.audioEnabled)
        XCTAssertEqual(frames, [1], "a reused ID does not preserve the previous user's enable consent")
    }

    func testMalformedFormatsDuplicatesAndOlderSequencesAreDiscarded() throws {
        let media = connectedMedia()
        try enable(media)
        var frames: [UInt64] = []
        media.onAudioFrame = { frames.append($0.sequence) }
        media.consumePCM(Data(count: 959), streamID: 42, sequence: 1, sampleRate: 48_000, channels: 1)
        media.consumePCM(Data(count: 961), streamID: 42, sequence: 2, sampleRate: 48_000, channels: 1)
        media.consumePCM(Data(count: 960), streamID: 42, sequence: 3, sampleRate: 44_100, channels: 1)
        media.consumePCM(Data(count: 960), streamID: 42, sequence: 4, sampleRate: 48_000, channels: 2)
        acceptPCM(media, sequence: 10)
        acceptPCM(media, sequence: 10)
        acceptPCM(media, sequence: 9)
        try acceptState(media, available: true, enabled: true) // duplicate acknowledgement must not reset ordering
        acceptPCM(media, sequence: 8)
        acceptPCM(media, sequence: 11)
        XCTAssertEqual(frames, [10, 11])
        media.reset()
    }

    func testUnavailableAndRemoteAudioErrorStopPlaybackWithoutEndingSession() throws {
        for useError in [false, true] {
            let media = connectedMedia()
            try enable(media)
            var stops = 0
            var frames = 0
            media.onAudioStateChanged = { _, enabled in if !enabled { stops += 1 } }
            media.onAudioFrame = { _ in frames += 1 }
            if useError { media.consumeSession(Data(#"{"kind":"audio_error","message":"unavailable"}"#.utf8), streamID: 42) }
            else { try acceptState(media, available: false, enabled: false) }
            acceptPCM(media, sequence: 1)
            XCTAssertFalse(media.audioEnabled)
            XCTAssertFalse(media.audioPending)
            XCTAssertNotNil(media.audioError)
            XCTAssertEqual(stops, 1)
            XCTAssertEqual(frames, 0)
            XCTAssertTrue(media.isConnected)
            if !useError {
                XCTAssertFalse(media.setAudioEnabled(true))
                try acceptState(media, available: true, enabled: false)
                XCTAssertNil(media.audioError, "restored availability must clear the obsolete unavailable notice")
                XCTAssertFalse(media.audioEnabled, "availability alone never restores user consent")
            }
            media.reset()
        }
    }

    func testPlaybackFailureStopsLocalFramesAndRequestsRemoteOff() throws {
        let media = connectedMedia()
        try enable(media)
        var actions: [ControllerMediaAction] = []
        var frames = 0
        media.performAction = { actions.append($0); return true }
        media.onAudioFrame = { _ in frames += 1 }
        media.reportPlaybackFailure()
        acceptPCM(media, sequence: 1)
        XCTAssertEqual(actions, [.setAudio(streamID: 42, enabled: false)])
        XCTAssertFalse(media.audioEnabled)
        XCTAssertFalse(media.audioPending)
        XCTAssertNotNil(media.audioError)
        XCTAssertEqual(frames, 0)
        try acceptState(media, available: true, enabled: true)
        XCTAssertFalse(media.audioEnabled, "an old enabled state cannot recover a failed native output")
        try enable(media)
        acceptPCM(media, sequence: 1)
        XCTAssertEqual(frames, 1)
        XCTAssertNil(media.audioError)
        media.reset()
    }

    func testRejectedEnableSubmissionDoesNotGrantPlaybackConsent() throws {
        let media = connectedMedia()
        media.performAction = { _ in false }
        XCTAssertFalse(media.setAudioEnabled(true))
        XCTAssertFalse(media.audioPending)
        XCTAssertNotNil(media.audioError)
        try acceptState(media, available: true, enabled: true)
        XCTAssertFalse(media.audioEnabled)
        XCTAssertFalse(media.setAudioEnabled(false))
        XCTAssertFalse(media.audioEnabled)
    }

    func testMalformedOrRetiredSessionEventsCannotChangeAudioState() throws {
        let media = connectedMedia()
        XCTAssertTrue(media.setAudioEnabled(true))
        for data in [Data("not json".utf8), Data(#"{"kind":"audio_state","available":true}"#.utf8),
                     Data(#"{"kind":"audio_state","available":true,"enabled":"true"}"#.utf8), Data(count: 16_385)] {
            media.consumeSession(data, streamID: 42)
        }
        try acceptState(media, streamID: 41, available: true, enabled: true)
        XCTAssertTrue(media.audioPending)
        XCTAssertNil(media.audioAvailable)
        XCTAssertFalse(media.audioEnabled)
        media.reset()
    }

    private func connectedMedia() -> ControllerMediaBridge {
        let media = ControllerMediaBridge()
        media.attach(streamID: 42)
        media.performAction = { _ in true }
        return media
    }

    private func enable(_ media: ControllerMediaBridge, streamID: UInt64 = 42) throws {
        try acceptState(media, streamID: streamID, available: true, enabled: false)
        XCTAssertTrue(media.setAudioEnabled(true))
        try acceptState(media, streamID: streamID, available: true, enabled: true)
        XCTAssertTrue(media.audioEnabled)
    }

    private func acceptState(_ media: ControllerMediaBridge, streamID: UInt64 = 42, available: Bool, enabled: Bool) throws {
        let data = try JSONSerialization.data(withJSONObject: ["kind": "audio_state", "available": available, "enabled": enabled])
        media.consumeSession(data, streamID: streamID)
    }

    private func acceptPCM(_ media: ControllerMediaBridge, streamID: UInt64 = 42, sequence: UInt64) {
        media.consumePCM(Data(count: 960), streamID: streamID, sequence: sequence, sampleRate: 48_000, channels: 1)
    }
}
