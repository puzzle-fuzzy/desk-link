import DeskLinkAppleCore
import XCTest

@MainActor
final class IOSLifecycleTests: XCTestCase {
    func testBackgroundReleasesInputAndClearsDisplayedFrame() {
        let bridge = ControllerBridge.testing(
            configuration: DeskLinkRuntimeConfiguration(
                relayURL: "quic://127.0.0.1:4433",
                relayServerName: "localhost",
                platform: .ios
            ),
            state: .connected(streamID: 7)
        )

        bridge.suspendForBackground()
        bridge.suspendForBackground()

        XCTAssertNil(bridge.latestPixelBuffer)
        XCTAssertNil(bridge.videoSize)
        XCTAssertEqual(bridge.activeStreamIDForTesting, 0)
        XCTAssertEqual(bridge.releaseAllCallCountForTesting, 1)
    }

    func testForegroundStreamCannotBeReplacedByRetiredStream() {
        let bridge = ControllerBridge.testing(state: .reconnecting)

        bridge.acceptStateForTesting(streamID: 8)
        bridge.acceptStateForTesting(streamID: 7)

        XCTAssertEqual(bridge.activeStreamIDForTesting, 8)
        XCTAssertEqual(bridge.state, ConnectionState.connected(streamID: 8))
    }
}
