# DeskLink 应用账号登录实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use `executing-plans` to implement this plan task-by-task with review checkpoints.

**Goal:** 为 DeskLink 增加邮箱注册、邮箱验证、登录、退出登录、忘记密码、重置密码和同账号多设备登记，同时保持远程连接安全链路完全独立。

**Architecture:** 新增独立的应用账号控制面，负责用户资料、账号会话和本机应用设备登记；远程连接继续使用现有的设备密钥、配对邀请、主机审批、Noise 和 relay session。账号服务不接收远程连接密钥、不参与 Noise 握手、不把账号 Token 发送给 relay 或远程主机。每个安装实例拥有独立的本机应用设备记录，新设备登录不会复制其他设备的连接权限。

**Tech Stack:** Bun 1.3 + TypeScript + `bun:sqlite` 账号服务；平台客户端使用 Tauri Rust/Windows DPAPI、macOS/iOS Keychain；邮箱投递通过注入式邮件端口，开发环境使用文件/内存出口，生产环境要求配置 HTTPS 邮件 provider。

## Global Constraints

- 登录是所有 DeskLink UI 功能的前置条件，但账号登录不改变已建立的远程连接认证。
- 新设备登录不继承其他设备的已保存连接、审批权限、relay authentication、邀请或自动重连状态。
- 退出登录先断开当前远程会话并执行 `ReleaseAll`，然后清理本机连接材料和账号会话。
- 退出登录不自动撤销目标 Windows/macOS 主机上的可信控制端；撤销仍由主机端单独执行。
- 账号服务只能保存账号与应用设备元数据，不保存远程连接私钥、relay authentication、PairingInvite 原文或主机信任密钥。
- 账号 API 对注册、登录、重置密码和邮箱验证使用稳定错误码，不能通过响应差异枚举邮箱是否存在。
- 账号密码使用 Argon2id，密码重置和邮箱验证 Token 只保存哈希值且只能消费一次。
- 所有账号 Token 只保存在平台安全存储；不得写入 `UserDefaults`、`localStorage`、诊断日志或远程协议。

---

### Task 1: 建立账号服务边界与 Bun 工程

**Files:**
- Create: `server/account/package.json`
- Create: `server/account/tsconfig.json`
- Create: `server/account/src/app.ts`
- Create: `server/account/src/config.ts`
- Create: `server/account/src/http.ts`
- Create: `server/account/src/domain/errors.ts`
- Test: `server/account/src/app.test.ts`

**Interfaces:**
- `createAccountApp(dependencies: AccountAppDependencies): AccountApp`
- `AccountApp.fetch(request: Request): Promise<Response>`
- `GET /health` 返回 `{ schema: 1, status: "ok", service: "desklink-account" }`。
- 未知路径返回稳定的 `{ error: "not_found" }`，响应带 `no-store`、`nosniff` 和请求 ID。

**Steps:**
- [ ] 为 health、未知路径、错误 JSON、安全响应头编写失败测试。
- [ ] 运行 `cd server/account && bun test src/app.test.ts`，确认测试失败。
- [ ] 实现依赖注入的 app factory、配置读取、JSON 响应、请求 ID、CORS allow-list 和错误转换。
- [ ] 再次运行 focused test，确认通过。
- [ ] 提交 `feat(account): add account service boundary`。

### Task 2: 实现账号数据库、密码和一次性 Token

**Files:**
- Create: `server/account/src/db/migrations/001_account.sql`
- Create: `server/account/src/db/store.ts`
- Create: `server/account/src/db/types.ts`
- Create: `server/account/src/domain/password.ts`
- Create: `server/account/src/domain/tokens.ts`
- Test: `server/account/src/db/store.test.ts`
- Test: `server/account/src/domain/password.test.ts`
- Test: `server/account/src/domain/tokens.test.ts`

**Database tables:**
- `users`: email、Argon2id password hash、邮箱验证时间、状态、时间戳。
- `account_devices`: user、platform、name、createdAt、lastSeenAt、revokedAt。
- `account_sessions`: user、device、refresh token hash、session family、过期和撤销状态。
- `action_tokens`: 验证邮箱与重置密码的 token hash、用途、过期和消费时间。
- `audit_events`: 脱敏动作、用户/设备 ID 和时间。

**Interfaces:**
- `AccountStore.createUser`, `findUserByEmail`, `createDevice`, `rotateSession`。
- `AccountStore.consumeActionToken(hash, purpose, now)`。
- `hashPassword`, `verifyPassword`。
- `createOpaqueToken(bytes = 32): { value: string; hash: string }`。

**Steps:**
- [ ] 测试邮箱标准化、密码边界、Argon2id、过期/错误用途/重复消费 Token。
- [ ] 运行 focused tests，确认失败。
- [ ] 使用 SQLite WAL、外键、事务和索引实现 store。
- [ ] Token 使用 CSPRNG，数据库只保存 SHA-256 hash；refresh rotation 和 Token 消费使用事务。
- [ ] 运行 focused tests，确认通过。
- [ ] 提交 `feat(account): add durable account and session storage`。

### Task 3: 完成邮箱注册、验证、登录、退出和密码重置 API

**Files:**
- Create: `server/account/src/services/account-service.ts`
- Create: `server/account/src/services/session-service.ts`
- Create: `server/account/src/providers/mailer.ts`
- Create: `server/account/src/providers/dev-mailer.ts`
- Create: `server/account/src/providers/http-mailer.ts`
- Modify: `server/account/src/app.ts`
- Test: `server/account/src/services/account-service.test.ts`
- Test: `server/account/src/services/session-service.test.ts`
- Test: `server/account/src/http-auth.test.ts`

**HTTP contract:**
- `POST /v1/account/register` 接收 email、password、platform、device name，始终返回 202。
- `POST /v1/account/verify-email` 消费一次性 token。
- `POST /v1/account/login` 验证已确认邮箱并登记当前应用设备，返回短期 access token 和可轮换 refresh token。
- `POST /v1/account/refresh` 轮换 refresh token；replay 撤销整个 session family。
- `POST /v1/account/logout` 只撤销当前账号会话。
- `POST /v1/account/password/forgot` 对未知和已知邮箱返回完全相同的 202。
- `POST /v1/account/password/reset` 消费 token、设置新密码并撤销账号全部会话。
- `GET /v1/account/me`、`GET /v1/account/devices`、`DELETE /v1/account/devices/:deviceId` 管理账号应用设备。

**Steps:**
- [ ] 测试注册、开发邮件出口、邮箱验证、登录、刷新、退出、忘记密码和重置密码。
- [ ] 测试未验证邮箱、错误密码、过期 Token、重复消费、限流和 session family replay。
- [ ] 实现 15 分钟 access token、30 天 refresh token、密码重置后的全会话撤销和稳定错误码。
- [ ] 开发环境邮件写入临时出口；生产环境的 HTTPS provider 要求配置 `DESKLINK_ACCOUNT_MAIL_URL`、`DESKLINK_ACCOUNT_MAIL_TOKEN` 和 `DESKLINK_ACCOUNT_MAIL_FROM`。
- [ ] 运行 `cd server/account && bun test`，确认全部通过。
- [ ] 提交 `feat(account): add email account lifecycle APIs`。

### Task 4: 邮件 outbox、限流、审计和运维

**Files:**
- Create: `server/account/src/workers/email-outbox.ts`
- Create: `server/account/src/security/rate-limiter.ts`
- Create: `server/account/src/security/audit.ts`
- Modify: `server/account/src/db/migrations/001_account.sql`
- Create: `server/account/Dockerfile`
- Create: `server/account/README.md`
- Modify: `deploy/tencent-relay/compose.yml`
- Test: `server/account/src/workers/email-outbox.test.ts`
- Test: `server/account/src/security/rate-limiter.test.ts`

**Steps:**
- [ ] 测试重复请求去重、租约恢复、最大重试、注册/登录/重置限流和过期桶清理。
- [ ] 实现 queued/running/sent/retry/dead 状态、claim/heartbeat/sweep/cancel 语义。
- [ ] 审计只保存脱敏动作，不保存密码、Token、relay authentication 或远程密钥。
- [ ] 增加 `/ready`、结构化日志、数据库备份说明、邮件 provider 配置和 SQLite 单实例限制。
- [ ] 运行 `cd server/account && bun test && bunx tsc --noEmit`。
- [ ] 提交 `feat(account): add email delivery and operational safeguards`。

### Task 5: Apple 账号会话、本机链接边界和 iOS 登录

**Files:**
- Create: `apps/apple/Sources/DeskLinkAppleCore/AccountSession.swift`
- Create: `apps/apple/Sources/DeskLinkAppleCore/AccountAPI.swift`
- Modify: `apps/apple/Sources/DeskLinkAppleCore/KeychainStore.swift`
- Modify: `apps/apple/Sources/DeskLinkAppleCore/SavedHostStore.swift`
- Modify: `apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift`
- Create: `apps/ios/DeskLinkIOS/Account/IOSAccountRootView.swift`
- Create: `apps/ios/DeskLinkIOS/Account/IOSAccountViewModel.swift`
- Modify: `apps/ios/DeskLinkIOS/DeskLinkIOSApp.swift`
- Modify: `apps/ios/DeskLinkIOS/Views/IOSRootView.swift`
- Test: `apps/apple/Tests/DeskLinkAppleCoreTests/AccountSessionTests.swift`
- Test: `apps/ios/DeskLinkIOSTests/IOSAccountPresentationTests.swift`
- Test: `apps/ios/DeskLinkIOSUITests/IOSAccountFlowUITests.swift`

**Rules:**
- 账号 Token 使用独立 Keychain service/account；新登录设备不读取历史 SavedHost records。
- 退出顺序固定为 `releaseAll -> disconnect -> clearSavedHosts -> clearAccountSession`。
- 设备身份密钥保留，但本机连接材料、邀请、relay authentication 和自动重连状态全部删除。
- iOS 未登录只能看到登录/注册/忘记密码；登录后才能进入连接和控制页面。
- 401 只允许一次 refresh；refresh 失败进入登录界面，不自动改动远程连接权限。

**Steps:**
- [ ] 先写 storage、logout transaction 和 presentation tests。
- [ ] 实现 URLSession API client、Keychain session store、AccountViewModel 和原生登录页面。
- [ ] 接入现有 iOS root navigation，保留现有扫码、直接触控、全屏控制和原生标题栏。
- [ ] 运行 AppleCore、iOS unit/UI tests。
- [ ] 提交 `feat(apple): add account session and iOS login gate`。

### Task 6: macOS 登录门禁和 Windows 登录门禁

**Files:**
- Create: `apps/macos/Sources/DeskLinkApp/Views/AccountView.swift`
- Create: `apps/macos/Sources/DeskLinkApp/Account/MacAccountViewModel.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/DeskLinkApp.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Views/DeskLinkStyle.swift`
- Create: `apps/windows/src/account.rs`
- Create: `apps/windows-ui/src-tauri/src/account.rs`
- Modify: `apps/windows-ui/src-tauri/src/lib.rs`
- Modify: `apps/windows-ui/src/main.ts`
- Modify: `apps/windows-ui/src/navigation.ts`
- Test: `apps/macos/Tests/DeskLinkAppTests/AccountPresentationTests.swift`
- Test: `apps/windows/src/account_tests.rs`
- Test: `apps/windows-ui/src/account.test.ts`

**Rules:**
- 未登录不启动 controller/host runtime，也不显示可执行的远程操作。
- Windows refresh token 只进 DPAPI，不进入 WebView、localStorage 或 diagnostics。
- macOS 使用 Keychain；账号状态与 Accessibility/Screen Recording 状态分开显示。
- 退出先停止当前会话并清空本机 saved devices，再清除账号会话；不自动撤销主机可信控制端。

**Steps:**
- [ ] 写三端状态和退出测试。
- [ ] 实现 macOS SwiftUI 账号页、Windows DPAPI session store、Tauri commands 和登录导航门禁。
- [ ] 运行 `cd apps/macos && swift test --arch arm64`、`cargo test -p desklink-windows` 和 `cd apps/windows-ui && bun test`。
- [ ] 提交 `feat(account): gate all client workspaces behind login`。

### Task 7: 全仓验收、部署说明和生产阻塞项

**Files:**
- Create: `docs/account-login.md`
- Create: `docs/account-data-model.md`
- Create: `docs/account-release-runbook.md`
- Modify: `README.md`
- Modify: `PRODUCT.md`
- Modify: `DESIGN.md`
- Create: `scripts/verify-account-service.sh`

**Steps:**
- [ ] 增加注册/验证/登录/刷新/退出/重置、多设备独立权限、退出清理、邮件失败和数据库恢复验收矩阵。
- [ ] 运行 Rust workspace、Bun account/diagnostics、AppleCore、macOS arm64、iOS simulator 和 Windows portable tests。
- [ ] 没有真实邮件 provider、域名、TLS、数据库备份和部署密钥时，只报告代码与本地测试完成，不报告线上注册/找回密码已经可用。
- [ ] 检查暂存区后提交 `feat(account): complete application login rollout` 并推送 `main`。

## Deliberate non-goals

- 不把邮箱密码、账号 Token、用户 ID 加入 relay join、PairingInvite 或 Noise payload。
- 不让同账号登录自动获得其他设备曾经审批过的主机权限。
- 不把远程连接密钥上传账号服务器。
- 第一版不跨设备同步包含 `relay_authentication`、邀请原文或主机私钥的连接保险库；新设备需要重新配对。
- 不把账号退出实现为撤销目标主机可信控制端；两者是不同的高风险动作。
