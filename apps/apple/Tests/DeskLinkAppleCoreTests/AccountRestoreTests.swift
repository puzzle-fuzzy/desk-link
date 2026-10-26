import Foundation
import XCTest
@testable import DeskLinkAppleCore

// Purely synthetic transport and Keychain data. These tests never contact an
// account service or use the system Keychain.
@MainActor
final class AccountRestoreTests: XCTestCase {
    func testTransientMeFailuresPreserveBlobAndNeverAttemptRefresh() async throws {
        let failures: [AccountTestTransport.Reply] = [
            .failure(.notConnectedToInternet), .failure(.cancelled),
            .response(500, errorBody("internal")), .response(429, errorBody("rate_limited")),
            .response(401, errorBody("unauthorized")), .response(200, Data("{}".utf8)),
        ]
        for failure in failures {
            let store = AccountTestKeychain(session: sessionBody())
            let transport = AccountTestTransport([failure])
            let client = makeClient(store, transport)
            await client.restore()
            XCTAssertEqual(store.sessionData, sessionBody())
            XCTAssertEqual(store.deleteAttempts, 0)
            XCTAssertEqual(client.state, .offline(userA))
            let paths = await transport.paths()
            XCTAssertEqual(paths, ["/v1/account/me"])
        }
    }

    func testTransientOrMalformedRefreshResponsePreservesSavedSession() async {
        for reply in [AccountTestTransport.Reply.failure(.timedOut), .response(503, errorBody("token_invalid")), .response(429, errorBody("rate_limited")), .response(200, Data("{}".utf8))] {
            let store = AccountTestKeychain(session: sessionBody())
            let transport = AccountTestTransport([.response(401, errorBody("token_invalid")), reply])
            let client = makeClient(store, transport)
            await client.restore()
            XCTAssertEqual(store.sessionData, sessionBody())
            XCTAssertEqual(store.deleteAttempts, 0)
            XCTAssertEqual(client.state, .offline(userA))
        }
    }

    func testOnlyExplicitInvalidRefreshTokenClearsLocalSession() async {
        let store = AccountTestKeychain(session: sessionBody())
        let transport = AccountTestTransport([
            .response(401, errorBody("token_invalid")), .response(401, errorBody("token_invalid")),
        ])
        let client = makeClient(store, transport)
        await client.restore()
        XCTAssertEqual(client.state, .signedOut)
        XCTAssertNil(store.sessionData)
        XCTAssertEqual(store.deleteAttempts, 1)
    }

    func testOfflineSessionCanRetryWithoutEnteringCredentialsAgain() async {
        let store = AccountTestKeychain(session: sessionBody())
        let transport = AccountTestTransport([.failure(.notConnectedToInternet), .response(200, meBody())])
        let client = makeClient(store, transport)
        await client.restore()
        XCTAssertEqual(client.state, .offline(userA))
        await client.restore()
        XCTAssertEqual(client.state, .signedIn(userA))
        XCTAssertNil(client.lastError)
        XCTAssertEqual(store.deleteAttempts, 0)
        let paths = await transport.paths()
        XCTAssertEqual(paths, ["/v1/account/me", "/v1/account/me"])
    }

    func testConcurrentRestoreIsSingleFlight() async {
        let gate = AccountTestGate()
        let store = AccountTestKeychain(session: sessionBody())
        let transport = AccountTestTransport([.suspended(200, meBody(), gate)])
        let client = makeClient(store, transport)
        let first = Task { await client.restore() }
        await gate.waitUntilEntered()
        await client.restore()
        let paths = await transport.paths()
        XCTAssertEqual(paths.count, 1)
        await gate.release()
        await first.value
        XCTAssertEqual(client.state, .signedIn(userA))
        XCTAssertFalse(client.isRestoring)
    }

    func testLogoutPreventsLateMeFromRestoringDeletedSession() async {
        let gate = AccountTestGate()
        let store = AccountTestKeychain(session: sessionBody())
        let transport = AccountTestTransport([.suspended(200, meBody(), gate), .response(200, Data("{}".utf8))])
        let client = makeClient(store, transport)
        let pending = Task { await client.restore() }
        await gate.waitUntilEntered()
        await client.logout()
        await gate.release()
        await pending.value
        XCTAssertEqual(client.state, .signedOut)
        XCTAssertNil(store.sessionData)
    }

    func testNewLoginSurvivesOldRefreshSuccessAndInvalidation() async {
        for status in [200, 401] {
            let gate = AccountTestGate()
            let reply = status == 200 ? refreshBody(prefix: "rotated-a") : errorBody("token_invalid")
            let store = AccountTestKeychain(session: sessionBody())
            let transport = AccountTestTransport([
                .response(401, errorBody("token_invalid")), .suspended(status, reply, gate),
                .response(200, loginBody()),
            ])
            let client = makeClient(store, transport)
            let old = Task { await client.restore() }
            await gate.waitUntilEntered()
            await client.login(email: userB.email, password: "synthetic-password-not-used")
            let newSession = store.sessionData
            await gate.release()
            await old.value
            XCTAssertEqual(client.state, .signedIn(userB))
            XCTAssertEqual(store.sessionData, newSession)
            XCTAssertEqual(store.deleteAttempts, 0)
        }
    }

    func testLateLogoutCannotDeleteNewLogin() async {
        let gate = AccountTestGate()
        let store = AccountTestKeychain(session: sessionBody())
        let transport = AccountTestTransport([
            .response(200, meBody()), .suspended(200, Data("{}".utf8), gate), .response(200, loginBody()),
        ])
        let client = makeClient(store, transport)
        await client.restore()
        let logout = Task { await client.logout() }
        await gate.waitUntilEntered()
        XCTAssertNil(store.sessionData)
        await client.login(email: userB.email, password: "synthetic-password-not-used")
        let newSession = store.sessionData
        await gate.release()
        await logout.value
        XCTAssertEqual(client.state, .signedIn(userB))
        XCTAssertEqual(store.sessionData, newSession)
    }

    func testSkipLoginInvalidatesPendingRestoreWithoutDeletingCredential() async {
        let gate = AccountTestGate()
        let store = AccountTestKeychain(session: sessionBody())
        let transport = AccountTestTransport([.suspended(200, meBody(), gate)])
        let client = makeClient(store, transport)
        let pending = Task { await client.restore() }
        await gate.waitUntilEntered()
        client.skipLogin()
        await gate.release()
        await pending.value
        XCTAssertEqual(client.state, .skipped)
        XCTAssertEqual(store.sessionData, sessionBody())
        XCTAssertEqual(store.deleteAttempts, 0)
    }

    func testCancelledRefreshEvenWithInvalidationResponseDoesNotDelete() async {
        let gate = AccountTestGate()
        let store = AccountTestKeychain(session: sessionBody())
        let transport = AccountTestTransport([
            .response(401, errorBody("token_invalid")), .suspended(401, errorBody("token_invalid"), gate),
        ])
        let client = makeClient(store, transport)
        let pending = Task { await client.restore() }
        await gate.waitUntilEntered()
        pending.cancel()
        await gate.release()
        await pending.value
        XCTAssertEqual(client.state, .offline(userA))
        XCTAssertEqual(store.sessionData, sessionBody())
        XCTAssertEqual(store.deleteAttempts, 0)
    }

    func testKeychainReadDecodeAndWriteErrorsNeverDelete() async {
        let unreadable = AccountTestKeychain(session: sessionBody())
        unreadable.failReads = true
        let client = makeClient(unreadable, AccountTestTransport([]))
        await client.restore()
        XCTAssertEqual(client.state, .offline(nil))
        XCTAssertEqual(unreadable.sessionData, sessionBody())
        XCTAssertEqual(unreadable.deleteAttempts, 0)

        let corrupt = AccountTestKeychain(session: Data("synthetic-invalid-json".utf8))
        await makeClient(corrupt, AccountTestTransport([])).restore()
        XCTAssertEqual(corrupt.sessionData, Data("synthetic-invalid-json".utf8))
        XCTAssertEqual(corrupt.deleteAttempts, 0)

        let unwritable = AccountTestKeychain(session: sessionBody())
        unwritable.failWrites = true
        let writing = makeClient(unwritable, AccountTestTransport([.response(200, meBody())]))
        await writing.restore()
        XCTAssertEqual(writing.state, .offline(userA))
        XCTAssertEqual(unwritable.sessionData, sessionBody())
        XCTAssertEqual(unwritable.deleteAttempts, 0)
    }

    func testFailedDeletionRemainsVisibleAndPreservesSavedBytes() async {
        let store = AccountTestKeychain(session: sessionBody())
        store.failDeletes = true
        let client = makeClient(store, AccountTestTransport([
            .response(401, errorBody("token_invalid")), .response(401, errorBody("token_invalid")),
        ]))
        await client.restore()
        XCTAssertEqual(client.state, .offline(userA))
        XCTAssertEqual(store.sessionData, sessionBody())
        XCTAssertNotNil(client.lastError)
    }

    func testRotatedTokensRemainInMemoryIfFirstPersistenceAttemptFails() async {
        let store = AccountTestKeychain(session: sessionBody())
        store.failWrites = true
        let transport = AccountTestTransport([
            .response(401, errorBody("token_invalid")), .response(200, refreshBody(prefix: "rotated-a")),
            .response(200, meBody()),
        ])
        let client = makeClient(store, transport)
        await client.restore()
        XCTAssertEqual(store.sessionData, sessionBody())
        store.failWrites = false
        await client.restore()
        let authorizations = await transport.authorizations()
        XCTAssertEqual(authorizations.last ?? nil, "Bearer rotated-a-access")
        XCTAssertEqual(client.state, .signedIn(userA))
        let paths = await transport.paths()
        XCTAssertEqual(paths.filter { $0.hasSuffix("refresh") }.count, 1)
    }

    func testOnlyStatusAndStableCodeDetermineInvalidation() {
        XCTAssertTrue(AccountClientError.http(status: 401, code: "token_invalid", message: "any localized text").isInvalidToken)
        XCTAssertFalse(AccountClientError.http(status: 503, code: "token_invalid", message: "登录状态已失效").isInvalidToken)
        XCTAssertFalse(AccountClientError.http(status: 401, code: "internal", message: "登录状态已失效").isInvalidToken)
        XCTAssertFalse(AccountClientError.server("token_invalid").isInvalidToken)
    }

    func testCancelledSuccessfulRotationRetainsReplacementEvenIfKeychainWriteFails() async throws {
        for failWrite in [false, true] {
            let gate = AccountTestGate()
            let store = AccountTestKeychain(session: sessionBody())
            store.failWrites = failWrite
            let transport = AccountTestTransport([
                .response(401, errorBody("token_invalid")), .suspended(200, refreshBody(prefix: "rotated-a"), gate),
                .response(200, meBody()),
            ])
            let client = makeClient(store, transport)
            let pending = Task { await client.restore() }
            await gate.waitUntilEntered()
            pending.cancel()
            await gate.release()
            await pending.value
            XCTAssertEqual(client.state, .offline(userA))
            XCTAssertEqual(store.deleteAttempts, 0)
            if failWrite {
                XCTAssertEqual(store.sessionData, sessionBody())
            } else {
                let saved = try XCTUnwrap(store.sessionData)
                let object = try XCTUnwrap(JSONSerialization.jsonObject(with: saved) as? [String: Any])
                let pair = try XCTUnwrap(object["tokens"] as? [String: Any])
                XCTAssertEqual(pair["refreshToken"] as? String, "rotated-a-refresh")
            }
            store.failWrites = false
            await client.restore()
            XCTAssertEqual(client.state, .signedIn(userA))
            let authorizations = await transport.authorizations()
            XCTAssertEqual(authorizations.last ?? nil, "Bearer rotated-a-access")
            let paths = await transport.paths()
            XCTAssertEqual(paths, ["/v1/account/me", "/v1/account/refresh", "/v1/account/me"])
        }
    }

    func testCancelledOldRotationCannotCheckpointOverNewLogin() async {
        let gate = AccountTestGate()
        let store = AccountTestKeychain(session: sessionBody())
        let transport = AccountTestTransport([
            .response(401, errorBody("token_invalid")), .suspended(200, refreshBody(prefix: "rotated-a"), gate),
            .response(200, loginBody()),
        ])
        let client = makeClient(store, transport)
        let pending = Task { await client.restore() }
        await gate.waitUntilEntered()
        pending.cancel()
        await client.login(email: userB.email, password: "synthetic-password-not-used")
        let replacement = store.sessionData
        await gate.release()
        await pending.value
        XCTAssertEqual(client.state, .signedIn(userB))
        XCTAssertEqual(store.sessionData, replacement)
        XCTAssertNil(client.lastError)
        XCTAssertEqual(store.deleteAttempts, 0)
    }

    func testCancelledOldRotationCannotResurrectLoggedOutCredential() async {
        let gate = AccountTestGate()
        let store = AccountTestKeychain(session: sessionBody())
        let transport = AccountTestTransport([
            .response(401, errorBody("token_invalid")), .suspended(200, refreshBody(prefix: "rotated-a"), gate),
            .response(200, Data("{}".utf8)),
        ])
        let client = makeClient(store, transport)
        let pending = Task { await client.restore() }
        await gate.waitUntilEntered()
        pending.cancel()
        await client.logout()
        await gate.release()
        await pending.value
        XCTAssertEqual(client.state, .signedOut)
        XCTAssertNil(store.sessionData)
    }

    private var userA: AccountUser { AccountUser(id: "synthetic-a", email: "a@example.invalid", emailVerified: true) }
    private var userB: AccountUser { AccountUser(id: "synthetic-b", email: "b@example.invalid", emailVerified: true) }

    private func makeClient(_ store: AccountTestKeychain, _ transport: AccountTestTransport) -> AccountClient {
        AccountClient(baseURL: URL(string: "https://account.example.invalid")!, platform: .macos, deviceName: "Synthetic Mac", keychain: store, transport: transport)
    }

    private func json(_ value: [String: Any]) -> Data { try! JSONSerialization.data(withJSONObject: value, options: .sortedKeys) }
    private func tokens(_ prefix: String) -> [String: Any] {
        ["tokenType": "Bearer", "accessToken": "\(prefix)-access", "refreshToken": "\(prefix)-refresh", "accessExpiresAt": 10, "refreshExpiresAt": 20]
    }
    private func userBody(_ user: AccountUser) -> [String: Any] { ["id": user.id, "email": user.email, "emailVerified": user.emailVerified] }
    private func sessionBody() -> Data { json(["user": userBody(userA), "deviceID": "synthetic-device", "tokens": tokens("synthetic-a")]) }
    private func meBody() -> Data { json(["user": userBody(userA)]) }
    private func loginBody() -> Data { json(["user": userBody(userB), "tokens": tokens("synthetic-b")]) }
    private func refreshBody(prefix: String) -> Data { json(["tokens": tokens(prefix)]) }
    private func errorBody(_ code: String) -> Data { json(["error": code, "message": "synthetic server response"]) }
}

private actor AccountTestGate {
    private var entered = false
    private var entranceWaiters: [CheckedContinuation<Void, Never>] = []
    private var continuation: CheckedContinuation<Void, Never>?

    func pause() async {
        entered = true
        for waiter in entranceWaiters { waiter.resume() }
        entranceWaiters.removeAll()
        await withCheckedContinuation { continuation = $0 }
    }

    func waitUntilEntered() async {
        if entered { return }
        await withCheckedContinuation { entranceWaiters.append($0) }
    }

    func release() { continuation?.resume(); continuation = nil }
}

private actor AccountTestTransport: AccountHTTPTransport {
    enum Reply: Sendable {
        case response(Int, Data)
        case failure(URLError.Code)
        case suspended(Int, Data, AccountTestGate)
    }
    private var replies: [Reply]
    private var requests: [URLRequest] = []

    init(_ replies: [Reply]) { self.replies = replies }
    func paths() -> [String] { requests.map { $0.url!.path } }
    func authorizations() -> [String?] { requests.map { $0.value(forHTTPHeaderField: "authorization") } }

    func data(for request: URLRequest) async throws -> (Data, URLResponse) {
        requests.append(request)
        guard !replies.isEmpty else { throw URLError(.badServerResponse) }
        let status: Int
        let body: Data
        switch replies.removeFirst() {
        case let .failure(code): throw URLError(code)
        case let .response(value, data): status = value; body = data
        case let .suspended(value, data, gate):
            await gate.pause()
            status = value; body = data
        }
        return (body, HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: nil)!)
    }
}

private final class AccountTestKeychain: KeychainStore, @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: Data] = [:]
    var failReads = false
    var failWrites = false
    var failDeletes = false
    private(set) var deleteAttempts = 0

    init(session: Data) { values["com.desklink.account-session/current"] = session }
    var sessionData: Data? { lock.withLock { values["com.desklink.account-session/current"] } }

    func read(service: String, account: String) throws -> Data? {
        try lock.withLock {
            if failReads { throw KeychainStoreError.status(-1) }
            return values["\(service)/\(account)"]
        }
    }
    func write(_ data: Data, service: String, account: String) throws {
        try lock.withLock {
            if failWrites { throw KeychainStoreError.status(-1) }
            values["\(service)/\(account)"] = data
        }
    }
    func delete(service: String, account: String) throws {
        try lock.withLock {
            deleteAttempts += 1
            if failDeletes { throw KeychainStoreError.status(-1) }
            values.removeValue(forKey: "\(service)/\(account)")
        }
    }
}
