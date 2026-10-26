import Foundation
import XCTest
@testable import DeskLinkAppleCore

@MainActor
final class ControllerMediaFileTests: XCTestCase {
    func testUploadRejectsUnsolicitedAndMismatchedRequestTransferDirectionOrStream() throws {
        let media = connectedMedia()
        try accept(media, id: idA, state: "completed")
        XCTAssertNil(media.fileTransfer)
        XCTAssertTrue(media.sendFile(URL(fileURLWithPath: "/tmp/中文.txt")))
        let expected = try XCTUnwrap(media.fileTransfer?.transferID)
        for (stream, id, request, direction) in [(UInt64(41), expected, UInt64(0), "upload"),
                                                  (42, idA, 0, "upload"), (42, expected, 1, "upload"),
                                                  (42, expected, 0, "download")] {
            try accept(media, stream: stream, id: id, request: request, direction: direction, state: "sending", transferred: 5)
        }
        XCTAssertEqual(media.fileTransfer?.state, .preparing)
        XCTAssertTrue(media.isFileBusy)
        try accept(media, id: expected, state: "sending", transferred: 5)
        XCTAssertEqual(media.fileTransfer?.transferred, 5)
        XCTAssertEqual(media.fileTransfer?.progress, 0.5)
        media.reset()
    }

    func testDownloadCorrelatesRequestAndLocksFirstValidTransferIdentity() throws {
        let media = connectedMedia()
        XCTAssertTrue(media.requestRemoteFile(to: URL(fileURLWithPath: "/tmp/下载", isDirectory: true)))
        let request = try XCTUnwrap(media.fileTransfer?.requestID)
        try accept(media, id: idA, request: request + 1, direction: "download", state: "receiving", transferred: 1)
        try accept(media, id: "not-a-transfer", request: request, direction: "download", state: "receiving", transferred: 1)
        XCTAssertEqual(media.fileTransfer?.state, .waiting)
        try accept(media, id: idA, request: request, direction: "download", state: "receiving", transferred: 2)
        XCTAssertEqual(media.fileTransfer?.transferID, idA)
        try accept(media, id: idB, request: request, direction: "download", state: "completed", transferred: 10, savedPath: "/tmp/下载/b.txt")
        XCTAssertTrue(media.isFileBusy)
        XCTAssertEqual(media.fileTransfer?.transferred, 2)
        try accept(media, id: idA, request: request, direction: "download", state: "completed", transferred: 10, savedPath: "/tmp/下载/a.txt")
        XCTAssertFalse(media.isFileBusy)
        XCTAssertEqual(media.fileTransfer?.savedPath, "/tmp/下载/a.txt")
    }

    func testTerminalDuplicatesAndLatePreviousRepliesCannotCompleteNextUpload() throws {
        let media = connectedMedia()
        XCTAssertTrue(media.sendFile(URL(fileURLWithPath: "/tmp/a")))
        let first = try XCTUnwrap(media.fileTransfer?.transferID)
        try accept(media, id: first, state: "completed", transferred: 10)
        XCTAssertFalse(media.isFileBusy)
        XCTAssertTrue(media.sendFile(URL(fileURLWithPath: "/tmp/b")))
        let second = try XCTUnwrap(media.fileTransfer?.transferID)
        XCTAssertNotEqual(first, second)
        try accept(media, id: first, state: "completed", transferred: 10)
        try accept(media, id: first, state: "failed", result: "timeout")
        XCTAssertEqual(media.fileTransfer?.transferID, second)
        XCTAssertEqual(media.fileTransfer?.state, .preparing)
        XCTAssertTrue(media.isFileBusy)
        XCTAssertNil(media.fileError)
        media.reset()
    }

    func testOutboundPathsMustBeAbsoluteFileURLsAndFitUTF8ByteLimit() {
        let media = connectedMedia()
        var actions: [ControllerMediaAction] = []
        media.performAction = { actions.append($0); return true }
        for url in [URL(string: "https://example.com/a")!, URL(string: "file:relative")!,
                    URL(fileURLWithPath: "/" + String(repeating: "中", count: 1366))] {
            XCTAssertFalse(media.sendFile(url))
            XCTAssertFalse(media.requestRemoteFile(to: url))
        }
        XCTAssertTrue(actions.isEmpty)
        let boundary = URL(fileURLWithPath: "/" + String(repeating: "中", count: 1365))
        XCTAssertEqual(boundary.path.utf8.count, 4096)
        XCTAssertTrue(media.sendFile(boundary))
        XCTAssertEqual(actions.count, 1)
        media.reset()
    }

    func testDownloadSavedPathAndProgressBoundsAreValidatedBeforePublishing() throws {
        let media = connectedMedia()
        XCTAssertTrue(media.requestRemoteFile(to: URL(fileURLWithPath: "/tmp")))
        let request = try XCTUnwrap(media.fileTransfer?.requestID)
        try accept(media, id: idA, request: request, direction: "download", state: "receiving", transferred: 1)
        for savedPath in ["relative.txt", "/" + String(repeating: "中", count: 1366)] {
            try accept(media, id: idA, request: request, direction: "download", state: "completed", transferred: 10, savedPath: savedPath)
        }
        try accept(media, id: idA, request: request, direction: "download", state: "receiving", transferred: 2, savedPath: "/tmp/a")
        try accept(media, id: idA, request: request, direction: "download", state: "receiving", transferred: 11)
        try accept(media, id: idA, request: request, direction: "download", state: "receiving", total: 256 * 1024 * 1024 + 1)
        try accept(media, id: idA, request: request, direction: "download", state: "receiving", name: String(repeating: "中", count: 342))
        XCTAssertEqual(media.fileTransfer?.transferred, 1)
        XCTAssertEqual(media.fileTransfer?.state, .receiving)
        XCTAssertTrue(media.isFileBusy)
        media.reset()
    }

    func testCancelWaitsForMatchingTerminalResultAndAllowsRetryAfterward() throws {
        let media = connectedMedia()
        var actions: [ControllerMediaAction] = []
        media.performAction = { actions.append($0); return true }
        XCTAssertFalse(media.cancelFileTransfer())
        XCTAssertTrue(media.sendFile(URL(fileURLWithPath: "/tmp/a")))
        let id = try XCTUnwrap(media.fileTransfer?.transferID)
        XCTAssertTrue(media.cancelFileTransfer())
        XCTAssertTrue(media.fileCancelPending)
        XCTAssertTrue(media.isFileBusy)
        XCTAssertFalse(media.cancelFileTransfer())
        XCTAssertEqual(actions.last, .cancelFile(streamID: 42))
        try accept(media, id: idA, state: "cancelled", result: "cancelled")
        XCTAssertTrue(media.fileCancelPending)
        try accept(media, id: id, state: "cancelled", result: "cancelled")
        XCTAssertFalse(media.isFileBusy)
        XCTAssertFalse(media.fileCancelPending)
        XCTAssertEqual(media.fileTransfer?.state, .cancelled)
        XCTAssertNotNil(media.fileError)
        XCTAssertTrue(media.sendFile(URL(fileURLWithPath: "/tmp/a")))
        XCTAssertNil(media.fileError)
        media.reset()
    }

    func testResetInvalidatesOldTransferAndDownloadRequestIDsAreNeverReused() throws {
        let media = connectedMedia()
        XCTAssertTrue(media.requestRemoteFile(to: URL(fileURLWithPath: "/tmp")))
        let firstRequest = try XCTUnwrap(media.fileTransfer?.requestID)
        media.reset()
        media.attach(streamID: 43)
        XCTAssertTrue(media.requestRemoteFile(to: URL(fileURLWithPath: "/tmp")))
        let nextRequest = try XCTUnwrap(media.fileTransfer?.requestID)
        XCTAssertGreaterThan(nextRequest, firstRequest)
        try accept(media, stream: 42, id: idA, request: firstRequest, direction: "download", state: "completed", transferred: 10, savedPath: "/tmp/old")
        try accept(media, stream: 43, id: idA, request: firstRequest, direction: "download", state: "completed", transferred: 10, savedPath: "/tmp/old")
        XCTAssertEqual(media.fileTransfer?.requestID, nextRequest)
        XCTAssertEqual(media.fileTransfer?.state, .waiting)
        XCTAssertTrue(media.isFileBusy)
        media.reset()
    }

    private let idA = String(repeating: "a", count: 32)
    private let idB = String(repeating: "b", count: 32)
    private func connectedMedia() -> ControllerMediaBridge {
        let media = ControllerMediaBridge(); media.attach(streamID: 42); media.performAction = { _ in true }; return media
    }
    private func accept(_ media: ControllerMediaBridge, stream: UInt64 = 42, id: String, request: UInt64 = 0,
                        direction: String = "upload", state: String, transferred: UInt64 = 0, total: UInt64 = 10,
                        result: String = "", savedPath: String? = nil, name: String = "中文.txt") throws {
        var object: [String: Any] = ["transfer_id": id, "request_id": request, "direction": direction, "state": state,
                                   "transferred": transferred, "total": total, "result": result, "name": name]
        if let savedPath { object["saved_path"] = savedPath }
        media.consumeFile(try JSONSerialization.data(withJSONObject: object), streamID: stream)
    }
}
