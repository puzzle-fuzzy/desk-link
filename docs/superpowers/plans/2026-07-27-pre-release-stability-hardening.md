# DeskLink 发布前稳定性加固计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use `subagent-driven-development` when this plan is executed by a separate session. This task is being executed inline in the current session, so each task will be implemented and verified here.

**Goal:** 在不改动 Windows UI 的前提下，消除 DeskLink 发布前影响连接成功率、生命周期安全、输入释放、视频稳定性和设备复用的高风险问题，并建立能覆盖关键边界的自动化验证门槛。

**Architecture:** 保持 Rust/FFI 为连接生命周期和安全材料的唯一事实来源；Apple Core 只负责平台适配、Keychain 持久化和用户可见状态；iOS/macOS UI 通过现有 Core 状态驱动，不把连接逻辑复制到视图层。所有新增超时、状态和持久化行为都使用可测试的纯函数或小型边界对象承载。

**Tech Stack:** Rust workspace (Tokio, QUIC, FFI), Swift Package Manager (Apple Core), SwiftUI/AppKit, Xcode iOS simulator tests, Bun account service, shell release verification.

## 全局约束

- 不修改 Windows UI；Rust 共享层只有在修复跨平台核心行为时才调整。
- 保留用户现有的 Xcode 工程元数据改动，不触碰 `apps/ios/DeskLinkIOS.xcodeproj/project.pbxproj`、`project.xcworkspace/` 和 `xcuserdata/`。
- 不把账号服务的本地可用性当作线上部署完成；邮件、数据库、域名和 TLS 仍需真实部署配置验证。
- 不记录链接码、密码、认证材料或视频内容；日志只允许稳定的脱敏标识。

## 执行顺序

### 1. P0：修复设备 ID + 密码连接的安全材料与邀请过期时间

**Files:** `crates/desklink-ffi/src/worker.rs`, `crates/desklink-ffi/src/lib.rs`, `apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift`。

- 将目录查询得到的 session/auth/host verify key 通过 FFI worker 的受控回调写入当前连接运行时，而不是只在 Rust 局部变量中使用。
- 将目录邀请的 `expires_at_unix_s` 传播到重连调度器，防止临时密码过期后无限重连。
- Apple Core 在目录连接成功后复用现有审批材料保存流程，确保连接进入最近设备且不会泄露秘密。
- 增加 Rust/Swift 测试：目录解析材料发布、过期时间传播、重连到期停止；覆盖错误输入和缺失字段。

**Acceptance:** 用设备 ID + 密码连接后可在当前设备的最近设备中复用；邀请过期后不再重连；旧的二维码/保存设备连接行为保持不变。

### 2. P0：为被控端审批增加有界生命周期

**Files:** `crates/desklink-ffi/src/host.rs`, `crates/desklink-ffi/src/host_worker.rs`。

- 为等待用户审批增加明确超时，并将超时归类为终止当前会话而非无限重试。
- 保持取消、断开、ReleaseAll 和 Closed 事件的顺序，避免超时路径留下输入状态或悬挂 QUIC 会话。
- 抽取可注入时长的等待函数，增加短时单元测试，验证审批通过、主动取消和审批超时三条路径。

**Acceptance:** 未审批的请求在有限时间后释放资源并显示可重试的失败状态；审批通过不受影响；退出应用或断开时仍能及时清理远端输入。

### 3. P0：加固控制端输入释放与关闭竞态

**Files:** `crates/desklink-ffi/src/worker.rs`, `crates/desklink-ffi/src/lib.rs`, relevant Rust tests。

- 审查有界输入队列在满载、重复 ReleaseAll、关闭和重连同时发生时的行为。
- 使 ReleaseAll 具备幂等且优先的投递保证；普通输入不能耗尽释放保留容量。
- 为队列满载、重复释放、关闭期间释放增加测试，确保 C FFI 返回值和本地 pressed 状态一致。

**Acceptance:** 任意断开/后台/窗口销毁路径都不会因普通输入拥塞而跳过释放；重复释放不产生错误噪音或残留按键。

### 4. P1：最近设备持久化去重与复用顺序

**Files:** `apps/apple/Sources/DeskLinkAppleCore/SavedHostStore.swift`, `ControllerBridge.swift`, Apple tests。

- 以稳定的 host identity（服务器、host verify key）去重，而不是每次审批都追加 UUID 记录；session ID 不参与去重，因为临时邀请在重新连接时可能轮换 session。
- 每次成功连接将设备移动到列表顶部，保留旧记录兼容性和 Keychain 删除行为。
- 增加重复保存、旧数据读取、删除和最近使用排序测试。

**Acceptance:** 最近成功连接的设备始终排在最上面；同一设备不会不断产生重复卡片；退出登录清空本机链接记录。

### 5. P1：视频解码与输入映射边界审查

**Files:** `apps/apple/Sources/DeskLinkAppleCore/H264Decoder.swift`, `H264AnnexB.swift`, iOS input/viewport tests。

- 对异常 NAL、空帧、非法 AVCC 长度和解码器重建增加安全保护，禁止不受信任视频数据触发强制解包崩溃。
- 检查主线程视频回调是否存在无界任务积压；在不改变显示语义的前提下保持最新帧优先。
- 维持当前已验证的 UIKit 左上角坐标、双指缩放锚点、轨迹板一比一移动和边缘跟随行为。

**Acceptance:** 异常视频包被丢弃并可恢复；渲染队列有界；现有 iOS 输入几何测试和 UI 流程继续通过。

### 6. P1：账号服务与本地免登录路径发布门槛

**Files:** `server/account/`, `scripts/verify-account-service.sh`, deployment documentation as needed。

- 保持“用户可跳过登录直接使用”的产品行为，登录只影响账号能力，不阻断本机连接。
- 补足账号服务启动配置、健康检查、邮件服务和持久化数据库的明确失败信息；不在客户端伪造注册成功。
- 以 Bun 测试/typecheck 和配置检查作为上线前门槛；真实 SMTP/数据库/域名/TLS 在部署环境单独 smoke test。

**Acceptance:** 无账号服务时客户端仍可免登录进入连接页；账号服务缺少生产配置时部署检查明确失败；注册、登录、退出和多设备会话行为有测试覆盖。

### 7. 发布验证与交付

- 运行 Rust workspace、Apple Core、macOS、账号服务测试和 typecheck。
- 运行 `scripts/verify-macos-runtime.sh` 与 `scripts/verify-ios.sh`；记录 simulator UI 测试中与当前产品行为不一致的旧断言并修正测试，而不是回退已确认的免登录设计。
- 执行 `git diff --check`，确认 Windows UI 和用户未授权的 Xcode 元数据没有进入提交。
- 生成单个有意义的稳定性提交并推送 `main`；最终报告修复项、未能在本地验证的线上依赖和精确验证结果。

## Self-review checklist

- 每个 P0/P1 项都有实现文件、边界条件、测试和验收标准。
- 计划没有把账号服务部署凭据假设为本地已有。
- 计划没有扩大到 Windows UI 或无关页面重构。
- 计划执行时若发现某项会改变协议兼容性，先停在该项并审查迁移方案，不直接破坏旧客户端。
