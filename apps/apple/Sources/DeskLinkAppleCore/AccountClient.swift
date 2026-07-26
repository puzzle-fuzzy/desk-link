import Foundation
import SwiftUI

public enum AccountPlatform: String, Codable, Sendable {
    case windows
    case macos
    case ios
}

public struct AccountUser: Codable, Equatable, Sendable {
    public let id: String
    public let email: String
    public let emailVerified: Bool

    public init(id: String, email: String, emailVerified: Bool) {
        self.id = id
        self.email = email
        self.emailVerified = emailVerified
    }
}

public enum AccountState: Equatable, Sendable {
    case loading
    case signedOut
    /// Local-only mode. It deliberately does not create or restore an account
    /// session and is used by clients that allow using the app without login.
    case skipped
    case signedIn(AccountUser)
}

public enum AccountClientError: LocalizedError, Equatable, Sendable {
    case invalidConfiguration
    case network(String)
    case server(String)
    case keychain(String)

    public var errorDescription: String? {
        switch self {
        case .invalidConfiguration: "账号服务地址无效，请联系管理员。"
        case let .network(message): message
        case let .server(message): message
        case let .keychain(message): message
        }
    }
}

private struct AccountTokens: Codable, Sendable {
    let tokenType: String
    let accessToken: String
    let refreshToken: String
    let accessExpiresAt: Int
    let refreshExpiresAt: Int
}

private struct StoredAccountSession: Codable, Sendable {
    var user: AccountUser
    var deviceID: String
    var tokens: AccountTokens
}

private struct LoginResponse: Codable {
    let user: AccountUser
    let tokens: AccountTokens
}

private struct MeResponse: Codable {
    let user: AccountUser
}

private struct APIErrorResponse: Decodable {
    let message: String?
}

@MainActor
public final class AccountClient: ObservableObject {
    @Published public private(set) var state: AccountState = .loading
    @Published public private(set) var lastError: String?
    @Published public private(set) var isBusy = false

    public let platform: AccountPlatform
    public let deviceName: String

    private let baseURL: URL
    private let keychain: any KeychainStore
    private let sessionService = "com.desklink.account-session"
    private let sessionAccount = "current"
    private let deviceService = "com.desklink.account-device"
    private let deviceAccount = "installation-id"
    private var storedSession: StoredAccountSession?

    public init(
        baseURL: URL,
        platform: AccountPlatform,
        deviceName: String,
        keychain: any KeychainStore = SystemKeychainStore()
    ) {
        self.baseURL = baseURL
        self.platform = platform
        self.deviceName = deviceName
        self.keychain = keychain
    }

    public func restore() async {
        guard storedSession == nil else { return }
        do {
            guard let data = try keychain.read(service: sessionService, account: sessionAccount) else {
                state = .signedOut
                return
            }
            storedSession = try JSONDecoder().decode(StoredAccountSession.self, from: data)
            guard let current = storedSession else {
                state = .signedOut
                return
            }
            do {
                let data = try await request(
                    path: "v1/account/me",
                    method: "GET",
                    body: nil,
                    accessToken: current.tokens.accessToken
                )
                let response = try JSONDecoder().decode(MeResponse.self, from: data)
                storedSession?.user = response.user
                try persistSession()
                state = .signedIn(response.user)
            } catch {
                let tokens = try await refreshTokens(current.tokens.refreshToken)
                _ = tokens
            }
        } catch {
            clearLocalSession()
            lastError = error.localizedDescription
        }
    }

    public func register(email: String, password: String) async {
        await runBusy {
            _ = try await self.request(
                path: "v1/account/register",
                method: "POST",
                body: [
                    "email": email,
                    "password": password,
                    "deviceId": try self.installationID(),
                    "platform": self.platform.rawValue,
                    "deviceName": self.deviceName,
                ],
                accessToken: nil
            )
        }
    }

    public func verifyEmail(token: String) async {
        await runBusy {
            _ = try await self.request(
                path: "v1/account/verify-email",
                method: "POST",
                body: ["token": token],
                accessToken: nil
            )
        }
    }

    public func login(email: String, password: String) async {
        await runBusy {
            let data = try await self.request(
                path: "v1/account/login",
                method: "POST",
                body: [
                    "email": email,
                    "password": password,
                    "deviceId": try self.installationID(),
                    "platform": self.platform.rawValue,
                    "deviceName": self.deviceName,
                ],
                accessToken: nil
            )
            let response = try JSONDecoder().decode(LoginResponse.self, from: data)
            let session = StoredAccountSession(
                user: response.user,
                deviceID: try self.installationID(),
                tokens: response.tokens
            )
            try self.save(session: session)
            self.storedSession = session
            self.state = .signedIn(response.user)
        }
    }

    public func resendVerification(email: String) async {
        await runBusy {
            _ = try await self.request(
                path: "v1/account/verify-email/resend",
                method: "POST",
                body: ["email": email],
                accessToken: nil
            )
        }
    }

    public func requestPasswordReset(email: String) async {
        await runBusy {
            _ = try await self.request(
                path: "v1/account/password/forgot",
                method: "POST",
                body: ["email": email],
                accessToken: nil
            )
        }
    }

    public func resetPassword(token: String, password: String) async {
        await runBusy {
            _ = try await self.request(
                path: "v1/account/password/reset",
                method: "POST",
                body: ["token": token, "password": password],
                accessToken: nil
            )
        }
    }

    /// Revokes the account session when possible. Local account state is
    /// cleared even if the network is unavailable, so logout never leaves a
    /// usable bearer token in Keychain.
    public func logout() async {
        let token = storedSession?.tokens.accessToken
        if let token {
            _ = try? await request(path: "v1/account/logout", method: "POST", body: [:], accessToken: token)
        }
        clearLocalSession()
    }

    /// Enters the local-only workspace without creating an account session.
    /// Remote-device pairing and host approval remain independent of account
    /// login, so skipping login must not alter saved remote connections.
    public func skipLogin() {
        guard !isBusy else { return }
        lastError = nil
        state = .skipped
    }

    public func clearError() {
        lastError = nil
    }

#if DEBUG
    public func activateForTesting(email: String = "test@example.com") {
        let user = AccountUser(id: "ui-test-user", email: email, emailVerified: true)
        storedSession = StoredAccountSession(
            user: user,
            deviceID: "ui-test-device",
            tokens: AccountTokens(
                tokenType: "Bearer",
                accessToken: "ui-test-access",
                refreshToken: "ui-test-refresh",
                accessExpiresAt: Int.max,
                refreshExpiresAt: Int.max
            )
        )
        state = .signedIn(user)
    }
#endif

    private func refreshTokens(_ refreshToken: String) async throws -> AccountTokens {
        let data = try await request(
            path: "v1/account/refresh",
            method: "POST",
            body: ["refreshToken": refreshToken],
            accessToken: nil
        )
        struct RefreshResponse: Codable { let tokens: AccountTokens }
        let response = try JSONDecoder().decode(RefreshResponse.self, from: data)
        guard var current = storedSession else { throw AccountClientError.server("登录状态已失效，请重新登录。") }
        current.tokens = response.tokens
        try save(session: current)
        storedSession = current
        state = .signedIn(current.user)
        return response.tokens
    }

    private func runBusy(_ operation: @escaping () async throws -> Void) async {
        guard !isBusy else { return }
        isBusy = true
        lastError = nil
        defer { isBusy = false }
        do {
            try await operation()
        } catch {
            lastError = error.localizedDescription
        }
    }

    private func request(
        path: String,
        method: String,
        body: [String: String]?,
        accessToken: String?
    ) async throws -> Data {
        guard baseURL.scheme != nil, baseURL.host != nil else {
            throw AccountClientError.invalidConfiguration
        }
        var request = URLRequest(url: baseURL.appendingPathComponent(path))
        request.timeoutInterval = 15
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "accept")
        if let body {
            request.setValue("application/json", forHTTPHeaderField: "content-type")
            request.httpBody = try JSONEncoder().encode(body)
        }
        if let accessToken { request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "authorization") }
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            guard let http = response as? HTTPURLResponse else {
                throw AccountClientError.network("账号服务返回了无效响应。")
            }
            guard (200..<300).contains(http.statusCode) else {
                let message = (try? JSONDecoder().decode(APIErrorResponse.self, from: data).message)
                    ?? "账号服务暂时不可用，请稍后重试。"
                throw AccountClientError.server(message)
            }
            return data
        } catch let error as AccountClientError {
            throw error
        } catch {
            let endpoint = request.url?.host.map { "（\($0)）" } ?? ""
            throw AccountClientError.network("账号服务暂时无法连接\(endpoint)，请检查网络后重试。")
        }
    }

    private func installationID() throws -> String {
        if let data = try keychain.read(service: deviceService, account: deviceAccount),
           let value = String(data: data, encoding: .utf8),
           !value.isEmpty
        {
            return value
        }
        let value = UUID().uuidString.lowercased()
        do {
            try keychain.write(Data(value.utf8), service: deviceService, account: deviceAccount)
        } catch {
            throw AccountClientError.keychain("无法保存本机账号设备标识。")
        }
        return value
    }

    private func save(session: StoredAccountSession) throws {
        do {
            try keychain.write(
                JSONEncoder().encode(session),
                service: sessionService,
                account: sessionAccount
            )
        } catch {
            throw AccountClientError.keychain("无法安全保存登录状态。")
        }
    }

    private func persistSession() throws {
        guard let storedSession else { return }
        try save(session: storedSession)
    }

    private func clearLocalSession() {
        try? keychain.delete(service: sessionService, account: sessionAccount)
        storedSession = nil
        state = .signedOut
    }
}

public struct AccountLoginView: View {
    @ObservedObject private var account: AccountClient
    @State private var email = ""
    @State private var password = ""
    @State private var confirmation = ""
    @State private var mode: Mode = .login
    @State private var notice: String?
    @State private var canResendVerification = false

    private enum Mode: String {
        case login = "登录"
        case register = "注册"
        case forgot = "找回密码"
    }

    private let allowsSkipLogin: Bool

    public init(account: AccountClient, allowsSkipLogin: Bool = false) {
        self.account = account
        self.allowsSkipLogin = allowsSkipLogin
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 5) {
                Text("DeskLink")
                    .font(.system(size: 28, weight: .bold))
                Text(mode == .login
                    ? (allowsSkipLogin ? "登录或跳过后开始使用远程控制" : "登录后开始使用远程控制")
                    : mode == .register ? "创建 DeskLink 账号" : "通过邮箱重置密码")
                    .foregroundStyle(.secondary)
            }

            Picker("操作", selection: $mode) {
                Text("登录").tag(Mode.login)
                Text("注册").tag(Mode.register)
                Text("找回密码").tag(Mode.forgot)
            }
            .pickerStyle(.segmented)

            TextField("邮箱", text: $email)
                .autocorrectionDisabled()

            if mode != .forgot {
                SecureField("密码（至少 12 个字符）", text: $password)
            }
            if mode == .register {
                SecureField("再次输入密码", text: $confirmation)
            }

            if let currentNotice = notice {
                VStack(alignment: .leading, spacing: 8) {
                    Text(currentNotice)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    if canResendVerification {
                        Button("重新发送验证邮件") {
                            Task { @MainActor in
                                await account.resendVerification(email: email)
                                if account.lastError == nil {
                                    notice = "验证邮件已重新发送，请检查收件箱。"
                                }
                            }
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(.tint)
                        .disabled(account.isBusy || email.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    }
                }
            }
            if let error = account.lastError {
                Text(error)
                    .font(.footnote)
                    .foregroundStyle(.red)
            }

            Button(action: submit) {
                HStack {
                    if account.isBusy { ProgressView().controlSize(.small) }
                    Text(actionTitle)
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .disabled(account.isBusy || !canSubmit)

            if mode == .login {
                Button("还没有账号？注册") { switchMode(.register) }
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
            } else if mode != .forgot {
                Button("忘记密码") { switchMode(.forgot) }
                    .buttonStyle(.plain)
                    .foregroundStyle(.secondary)
            }

            if allowsSkipLogin {
                Divider()
                    .padding(.vertical, 2)
                Button("跳过登录，直接使用") {
                    account.skipLogin()
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity)
                Text("登录仅用于账号管理，不影响远程设备连接。")
                    .font(.footnote)
                    .foregroundStyle(.tertiary)
                    .frame(maxWidth: .infinity)
                    .multilineTextAlignment(.center)
            }
        }
        .padding(28)
        .frame(maxWidth: 420)
        .background(.background)
    }

    private var actionTitle: String {
        switch mode {
        case .login: "登录"
        case .register: "注册并发送验证邮件"
        case .forgot: "发送重置邮件"
        }
    }

    private var canSubmit: Bool {
        guard !email.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return false }
        if mode == .forgot { return true }
        return !password.isEmpty && (mode != .register || password == confirmation)
    }

    private func switchMode(_ next: Mode) {
        mode = next
        notice = nil
        canResendVerification = false
        account.clearError()
    }

    private func submit() {
        Task { @MainActor in
            notice = nil
            switch mode {
            case .login:
                await account.login(email: email, password: password)
            case .register:
                await account.register(email: email, password: password)
                if account.lastError == nil {
                    notice = "验证邮件已发送，请完成邮箱验证后再登录。"
                    canResendVerification = true
                    mode = .login
                }
            case .forgot:
                await account.requestPasswordReset(email: email)
                if account.lastError == nil {
                    notice = "如果邮箱已注册，重置密码邮件会很快送达。"
                    canResendVerification = false
                    mode = .login
                }
            }
        }
    }
}
