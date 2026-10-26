import Foundation
import XCTest
@testable import DeskLinkAppleCore
@testable import DeskLinkApp

@MainActor
final class MacFileTransferControllerTests: XCTestCase {
    func testSuccessfulUploadsAdvanceOneAtATimeAfterTerminalConfirmation() async throws {
        let picker = StubFileTransferPicker(files: [fileA, fileB])
        let media = connectedMedia()
        var actions: [ControllerMediaAction] = []
        media.performAction = { actions.append($0); return true }
        let controller = MacFileTransferController(picker: picker)
        controller.attach(to: media)
        await controller.chooseFiles()
        XCTAssertEqual(actions.count, 1)
        XCTAssertEqual(controller.queuedFiles.map(\.url), [fileB])
        let firstID = try XCTUnwrap(media.fileTransfer?.transferID)
        try finish(media, state: "completed")
        await waitUntil { actions.count == 2 }
        XCTAssertEqual(controller.recentTransfers.first?.transferID, firstID)
        XCTAssertTrue(controller.queuedFiles.isEmpty)
        XCTAssertEqual(media.fileTransfer?.name, "second.txt")
        XCTAssertTrue(media.isFileBusy)
        try finish(media, state: "completed")
        await waitUntil { controller.recentTransfers.count == 2 }
        XCTAssertFalse(media.isFileBusy)
        XCTAssertFalse(controller.canRetry)
        XCTAssertFalse(controller.isQueuePaused)
        controller.detach()
    }

    func testFailurePausesQueueRetryUsesSameFileAndContinueStartsNext() async throws {
        let media = connectedMedia()
        var sentPaths: [String] = []
        media.performAction = { action in
            if case let .sendFile(_, _, path) = action { sentPaths.append(path) }
            return true
        }
        let controller = MacFileTransferController(picker: StubFileTransferPicker(files: [fileA, fileB]))
        controller.attach(to: media)
        await controller.chooseFiles()
        try finish(media, state: "failed", result: "timeout")
        await waitUntil { controller.isQueuePaused }
        XCTAssertEqual(sentPaths, [fileA.path])
        XCTAssertTrue(controller.canRetry)
        XCTAssertEqual(controller.queuedFiles.map(\.url), [fileB])
        controller.retry()
        XCTAssertEqual(sentPaths, [fileA.path, fileA.path])
        try finish(media, state: "completed")
        await waitUntil { controller.recentTransfers.count == 2 }
        XCTAssertEqual(sentPaths.count, 2, "the paused remainder waits for an explicit continue action")
        controller.resumeQueue()
        XCTAssertEqual(sentPaths, [fileA.path, fileA.path, fileB.path])
        XCTAssertFalse(controller.isQueuePaused)
        controller.detach()
        media.reset()
    }

    func testFailedSubmissionRetainsRetryAndContinueCanSkipFailedFile() async {
        let media = connectedMedia()
        var accept = false
        var sentPaths: [String] = []
        media.performAction = { action in
            if case let .sendFile(_, _, path) = action { sentPaths.append(path) }
            return accept
        }
        let controller = MacFileTransferController(picker: StubFileTransferPicker(files: [fileA, fileB]))
        controller.attach(to: media)
        await controller.chooseFiles()
        XCTAssertTrue(controller.isQueuePaused)
        XCTAssertTrue(controller.canRetry)
        XCTAssertNotNil(controller.localError)
        XCTAssertEqual(controller.queuedFiles.map(\.url), [fileB])
        accept = true
        controller.resumeQueue()
        XCTAssertEqual(sentPaths, [fileA.path, fileB.path])
        XCTAssertTrue(media.isFileBusy)
        controller.detach()
        media.reset()
    }

    func testAsyncOutgoingChoiceIsDiscardedAfterContextChanges() async {
        let picker = StubFileTransferPicker(files: [])
        picker.suspendOutgoing = true
        let media = connectedMedia()
        var actions: [ControllerMediaAction] = []
        media.performAction = { actions.append($0); return true }
        let controller = MacFileTransferController(picker: picker)
        controller.attach(to: media)
        let task = Task { await controller.chooseFiles() }
        await waitUntil { picker.outgoingContinuation != nil }
        media.reset(); media.attach(streamID: 43)
        picker.completeOutgoing([fileA])
        await task.value
        XCTAssertTrue(actions.isEmpty)
        XCTAssertTrue(controller.queuedFiles.isEmpty)
        XCTAssertNotNil(controller.localError)
        XCTAssertFalse(controller.isPicking)
        controller.detach()
    }

    func testAsyncOutgoingChoiceCannotCrossRebindingToDifferentPeer() async {
        let picker = StubFileTransferPicker(files: [])
        picker.suspendOutgoing = true
        let first = connectedMedia()
        let second = connectedMedia(streamID: 99)
        var secondActions: [ControllerMediaAction] = []
        second.performAction = { secondActions.append($0); return true }
        let controller = MacFileTransferController(picker: picker)
        controller.attach(to: first)
        let task = Task { await controller.chooseFiles() }
        await waitUntil { picker.outgoingContinuation != nil }
        controller.attach(to: second)
        picker.completeOutgoing([fileA])
        await task.value
        XCTAssertTrue(secondActions.isEmpty)
        XCTAssertTrue(controller.queuedFiles.isEmpty)
        controller.detach()
    }

    func testAsyncDestinationChoiceCannotCrossRebindingToDifferentPeer() async {
        let picker = StubFileTransferPicker(files: [])
        picker.suspendDestination = true
        let first = connectedMedia()
        let second = connectedMedia(streamID: 99)
        var actions: [ControllerMediaAction] = []
        first.performAction = { actions.append($0); return true }
        second.performAction = { actions.append($0); return true }
        let controller = MacFileTransferController(picker: picker)
        controller.attach(to: first)
        let task = Task { await controller.chooseDestinationAndRequest() }
        await waitUntil { picker.destinationContinuation != nil }
        controller.attach(to: second)
        picker.completeDestination(URL(fileURLWithPath: "/tmp/download", isDirectory: true))
        await task.value
        XCTAssertTrue(actions.isEmpty)
        XCTAssertFalse(controller.canRetry)
        controller.detach()
    }

    func testDisconnectClearsQueueAndRetryBeforeAnotherPeerConnects() async throws {
        let media = connectedMedia()
        var sends = 0
        media.performAction = { action in if case .sendFile = action { sends += 1 }; return true }
        let controller = MacFileTransferController(picker: StubFileTransferPicker(files: [fileA, fileB]))
        controller.attach(to: media)
        await controller.chooseFiles()
        let old = try XCTUnwrap(media.fileTransfer)
        media.reset(); media.attach(streamID: 99)
        XCTAssertTrue(controller.queuedFiles.isEmpty)
        XCTAssertFalse(controller.canRetry)
        XCTAssertNotNil(controller.localError)
        try finish(media, transfer: old, state: "completed", stream: 42)
        await settle()
        controller.resumeQueue()
        controller.retry()
        XCTAssertEqual(sends, 1)
        XCTAssertFalse(media.isFileBusy)
        controller.detach()
    }

    func testDeferredTerminalFromOldBindingCannotAdvanceOrAppearInNewPeer() async throws {
        let first = connectedMedia()
        let second = connectedMedia(streamID: 99)
        let controller = MacFileTransferController(picker: StubFileTransferPicker(files: [fileA]))
        controller.attach(to: first)
        await controller.chooseFiles()
        try finish(first, state: "completed")
        // The sink defers consume via Task; replace its binding before it runs.
        controller.attach(to: second)
        await settle()
        XCTAssertTrue(controller.recentTransfers.isEmpty)
        XCTAssertFalse(controller.canRetry)
        XCTAssertTrue(controller.queuedFiles.isEmpty)
        controller.detach()
    }

    func testCancelPausesRemainderAndDownloadFailureCanRetryChosenDestination() async throws {
        let media = connectedMedia()
        var actions: [ControllerMediaAction] = []
        media.performAction = { actions.append($0); return true }
        let picker = StubFileTransferPicker(files: [fileA, fileB])
        picker.directory = URL(fileURLWithPath: "/tmp/保存", isDirectory: true)
        let controller = MacFileTransferController(picker: picker)
        controller.attach(to: media)
        await controller.chooseFiles()
        controller.cancel()
        XCTAssertTrue(controller.isQueuePaused)
        XCTAssertEqual(actions.last, .cancelFile(streamID: 42))
        try finish(media, state: "cancelled", result: "cancelled")
        await waitUntil { controller.recentTransfers.count == 1 }
        XCTAssertEqual(controller.queuedFiles.count, 1)
        controller.clearQueue()
        await controller.chooseDestinationAndRequest()
        let request = try XCTUnwrap(media.fileTransfer?.requestID)
        try finish(media, state: "failed", result: "unsupported")
        await waitUntil { controller.recentTransfers.count == 2 }
        XCTAssertTrue(controller.canRetry)
        controller.retry()
        guard case let .requestFile(streamID, nextRequest, directory) = actions.last else { return XCTFail("missing download retry") }
        XCTAssertEqual(streamID, 42)
        XCTAssertGreaterThan(nextRequest, request)
        XCTAssertEqual(directory, picker.directory?.path)
        controller.detach()
        media.reset()
    }

    private let fileA = URL(fileURLWithPath: "/tmp/中文-first.txt")
    private let fileB = URL(fileURLWithPath: "/tmp/second.txt")
    private func connectedMedia(streamID: UInt64 = 42) -> ControllerMediaBridge {
        let media = ControllerMediaBridge(); media.attach(streamID: streamID); media.performAction = { _ in true }; return media
    }
    private func finish(_ media: ControllerMediaBridge, transfer: RemoteFileTransfer? = nil, state: String,
                        result: String = "", stream: UInt64? = nil) throws {
        let transfer = try XCTUnwrap(transfer ?? media.fileTransfer)
        let object: [String: Any] = ["transfer_id": transfer.transferID, "request_id": transfer.requestID,
                                     "direction": transfer.direction.rawValue, "state": state,
                                     "transferred": 10, "total": 10, "result": result, "name": transfer.name]
        media.consumeFile(try JSONSerialization.data(withJSONObject: object), streamID: stream ?? media.streamID)
    }
    private func waitUntil(_ condition: () -> Bool, file: StaticString = #filePath, line: UInt = #line) async {
        for _ in 0..<100 {
            if condition() { return }
            await Task.yield()
        }
        XCTFail("deferred state did not arrive", file: file, line: line)
    }
    private func settle() async { for _ in 0..<10 { await Task.yield() } }
}

@MainActor
private final class StubFileTransferPicker: FileTransferPicker {
    var files: [URL]
    var directory: URL?
    var suspendOutgoing = false
    var suspendDestination = false
    var outgoingContinuation: CheckedContinuation<[URL], Never>?
    var destinationContinuation: CheckedContinuation<URL?, Never>?
    init(files: [URL]) { self.files = files }
    func outgoingFiles() async -> [URL] {
        if suspendOutgoing { return await withCheckedContinuation { outgoingContinuation = $0 } }
        return files
    }
    func destinationDirectory() async -> URL? {
        if suspendDestination { return await withCheckedContinuation { destinationContinuation = $0 } }
        return directory
    }
    func completeOutgoing(_ files: [URL]) { outgoingContinuation?.resume(returning: files); outgoingContinuation = nil }
    func completeDestination(_ directory: URL?) { destinationContinuation?.resume(returning: directory); destinationContinuation = nil }
}
