# Unified Remote Connection Workspace Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 macOS、Windows 的默认页面统一为“连接设备”工作台，把本机运行状态、权限、诊断和密钥信息降级到状态入口或设置/诊断页面，并为未来 iOS 控制端保留同一套交互契约。

**Architecture:** 保留现有 macOS SwiftUI 与 Windows Tauri/WebView 运行时接口，只重排页面层级和文案。macOS 将控制端工作台设为默认页面，共享本机和本机详情成为次级页面；Windows 使用同样的导航语义和默认控制端页面，但不修改 Rust/Tauri 远程内核。通过可单元测试的导航模型与 macOS 状态摘要函数锁定跨平台信息架构。

**Tech Stack:** Swift 6、SwiftUI、XCTest、TypeScript、Vanilla DOM、CSS、Bun test、Vite、Tauri 2

## Global Constraints

- 页面核心任务是“控制另一台设备”或“允许别人连接此设备”，首页不得以“本机运行状态”为首要内容。
- macOS、Windows、iOS 使用同一套任务名称、状态词和操作优先级；iOS 当前仓库没有客户端工程，本轮只维护设计契约。
- 本轮不修改 Windows 远程内核，不修改现有连接码、审批、权限检测、重连、冻结和关键帧请求协议。
- 本机运行指标、stream ID、密钥材料和完整权限明细只在设置/诊断或弹出详情中展示。
- 远程会话页保持沉浸式，被控画面优先；本机运行状态不进入会话页。
- 继续使用现有暖白、烧珊瑚色和语义状态色，不添加内部阴影、渐变或装饰性动效。
- macOS 验证使用 Apple Silicon arm64；iOS 不声明未执行的构建或运行验证。

---

## 文件地图

| 文件 | 责任 | 本计划中的变化 |
| --- | --- | --- |
| `apps/macos/Sources/DeskLinkApp/DeskLinkApp.swift` | macOS 应用入口与页面路由 | 默认进入连接页，新增共享/设备/设置映射和状态详情入口 |
| `apps/macos/Sources/DeskLinkApp/Views/DeskLinkStyle.swift` | macOS 视觉组件、主导航、按钮和状态样式 | 更新导航语义，增加可点击的本机状态摘要 |
| `apps/macos/Sources/DeskLinkApp/Views/DeskLinkHostStatus.swift` | macOS 本机状态摘要模型与详情视图 | 新建纯状态摘要映射和弹出详情内容 |
| `apps/macos/Sources/DeskLinkApp/Views/ControllerHomeView.swift` | macOS 控制端首页 | 改为“连接设备”工作台，保留最近设备，折叠诊断 |
| `apps/macos/Sources/DeskLinkApp/Views/ConnectView.swift` | macOS 连接码输入 | 支持一键粘贴、手动输入和明确的连接动作 |
| `apps/macos/Sources/DeskLinkApp/Views/HostHomeView.swift` | macOS 共享本机、批准设备和本机详情 | “本机连接”改成“共享此设备”，运行指标降为折叠诊断 |
| `apps/macos/Tests/DeskLinkAppTests/DeskLinkNavigationTests.swift` | macOS 页面契约测试 | 新建导航标签和本机状态摘要测试 |
| `apps/windows-ui/src/navigation.ts` | Windows 键盘导航和页面导航模型 | 导出统一导航项和内部页面映射 |
| `apps/windows-ui/src/navigation.test.ts` | Windows 导航单元测试 | 验证统一标签、默认页面映射和隐藏低频页面行为 |
| `apps/windows-ui/src/main.ts` | Windows 外壳、导航、共享本机页面 | 默认进入控制端，导航标签和右上角状态摘要同步 |
| `apps/windows-ui/src/controller.ts` | Windows 控制端页面 | 将标题、连接码入口和最近设备文案同步为统一体验 |
| `apps/windows-ui/src/styles.css` | Windows 页面布局和视觉样式 | 增加连接工作台、状态摘要和次级共享卡片样式 |
| `DESIGN.md` | 跨平台设计系统 | 更新信息架构、信息优先级和 iOS 约束 |
| `README.md` | 用户和开发者入口说明 | 更新 macOS 页面名称与当前 iOS 工程边界 |

---

### Task 1: 建立 macOS 统一导航和本机状态摘要

**Files:**
- Create: `apps/macos/Sources/DeskLinkApp/Views/DeskLinkHostStatus.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/DeskLinkApp.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Views/DeskLinkStyle.swift`
- Test: `apps/macos/Tests/DeskLinkAppTests/DeskLinkNavigationTests.swift`

**Interfaces:**
- Consumes: `HostState`、`HostBridge.lastError`、现有 `DeskLinkSection` 和 `DeskLinkShell`。
- Produces: `DeskLinkSection.connect/share/devices/settings`、`DeskLinkHostStatus`、`deskLinkHostStatus(for:lastError:)`，供应用入口和状态摘要使用。

- [ ] **Step 1: Write the failing macOS navigation and status tests**

创建 `DeskLinkNavigationTests.swift`，锁定用户可见导航和本机状态摘要，不测试 SwiftUI 布局细节：

```swift
import XCTest
@testable import DeskLinkApp

final class DeskLinkNavigationTests: XCTestCase {
    func testPrimaryNavigationUsesRemoteTasks() {
        XCTAssertEqual(
            DeskLinkSection.allCases.map(\.rawValue),
            ["连接设备", "共享此设备", "已批准设备", "设置 / 诊断"]
        )
        XCTAssertEqual(DeskLinkSection.connect.rawValue, "连接设备")
    }

    func testHostStatusSummaryKeepsRuntimeDetailsOutOfTheTopLevelCopy() {
        let summary = deskLinkHostStatus(for: .idle, lastError: nil)

        XCTAssertEqual(summary.title, "未开启共享")
        XCTAssertEqual(summary.detail, "需要共享这台 Mac 时生成连接码")
        XCTAssertFalse(summary.detail.contains("视频"))
        XCTAssertFalse(summary.detail.contains("关键帧"))
    }

    func testHostErrorTakesPriorityOverIdleState() {
        let summary = deskLinkHostStatus(for: .idle, lastError: "权限检查失败")

        XCTAssertEqual(summary.title, "需要处理")
        XCTAssertEqual(summary.detail, "打开设置检查本机共享权限")
    }
}
```

- [ ] **Step 2: Run the focused test and verify it fails**

Run:

```sh
cd apps/macos
swift test --arch arm64 --filter DeskLinkNavigationTests
```

Expected: FAIL because the new navigation cases and `deskLinkHostStatus` do not exist yet.

- [ ] **Step 3: Implement the semantic navigation model and status summary**

在 `DeskLinkStyle.swift` 将原导航替换为：

```swift
enum DeskLinkSection: String, CaseIterable, Identifiable {
    case connect = "连接设备"
    case share = "共享此设备"
    case devices = "已批准设备"
    case settings = "设置 / 诊断"

    var id: Self { self }
}
```

新建 `DeskLinkHostStatus.swift`，使用不泄露内部指标的摘要：

```swift
import SwiftUI

enum DeskLinkHostStatusTone: Equatable {
    case ready
    case attention
    case idle
    case working
}

struct DeskLinkHostStatus: Equatable {
    let title: String
    let detail: String
    let tone: DeskLinkHostStatusTone

    var systemImage: String {
        switch tone {
        case .ready: "checkmark.circle"
        case .attention: "exclamationmark.circle"
        case .idle: "circle"
        case .working: "arrow.triangle.2.circlepath"
        }
    }
}

func deskLinkHostStatus(for state: HostState, lastError: String?) -> DeskLinkHostStatus {
    if lastError != nil {
        return DeskLinkHostStatus(
            title: "需要处理",
            detail: "打开设置检查本机共享权限",
            tone: .attention
        )
    }

    switch state {
    case .idle, .closed:
        return DeskLinkHostStatus(
            title: "未开启共享",
            detail: "需要共享这台 Mac 时生成连接码",
            tone: .idle
        )
    case .connecting, .waitingForApproval, .negotiating, .stopping:
        return DeskLinkHostStatus(
            title: "连接中",
            detail: "正在准备本机共享",
            tone: .working
        )
    case .connected:
        return DeskLinkHostStatus(
            title: "本机可被连接",
            detail: "共享已准备好，等待远程控制请求",
            tone: .ready
        )
    case .failed:
        return DeskLinkHostStatus(
            title: "需要处理",
            detail: "打开设置查看共享错误",
            tone: .attention
        )
    }
}
```

- [ ] **Step 4: Update the shell and application route**

在 `DeskLinkApp.swift`：

```swift
@State private var section: DeskLinkSection = .connect

switch section {
case .connect:
    ControllerHomeView(bridge: controller)
case .share:
    HostHomeView(bridge: host, page: .connection)
case .devices:
    HostHomeView(bridge: host, page: .devices)
case .settings:
    HostHomeView(bridge: host, page: .overview)
}
```

把 `DeskLinkShell` 的右上角保护文本改为 `DeskLinkHostStatus` 摘要，并让摘要点击后打开 `DeskLinkHostStatusPopover(host: host, controller: controller)`。摘要必须仍然调用原有 `HostBridge` 和 `ControllerBridge`，不得触发新的运行时连接行为。

- [ ] **Step 5: Run the focused test and commit the navigation slice**

Run:

```sh
cd apps/macos
swift test --arch arm64 --filter DeskLinkNavigationTests
```

Expected: PASS with 3 tests. Commit:

```sh
git add apps/macos/Sources/DeskLinkApp/DeskLinkApp.swift \
  apps/macos/Sources/DeskLinkApp/Views/DeskLinkStyle.swift \
  apps/macos/Sources/DeskLinkApp/Views/DeskLinkHostStatus.swift \
  apps/macos/Tests/DeskLinkAppTests/DeskLinkNavigationTests.swift
git commit -m "feat(macos): make remote connection the primary workspace"
```

---

### Task 2: 完成 macOS 控制端和共享本机工作台

**Files:**
- Modify: `apps/macos/Sources/DeskLinkApp/Views/ControllerHomeView.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Views/ConnectView.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Views/HostHomeView.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Views/DeskLinkHostStatus.swift`

**Interfaces:**
- Consumes: Task 1 的 `DeskLinkSection`、`DeskLinkHostStatus` 和现有 `ControllerBridge.connect(invite:)`、`HostBridge.createInvite()`。
- Produces: macOS 默认连接工作台、共享本机次级入口、折叠式诊断入口；不改变任何 bridge 方法签名。

- [ ] **Step 1: Write the connection-workspace behavior checks**

在 `DeskLinkNavigationTests.swift` 追加纯文案和优先级测试：

```swift
func testConnectionWorkspaceCopyNamesTheRemoteTask() {
    XCTAssertEqual("连接设备", DeskLinkSection.connect.rawValue)
    XCTAssertEqual("共享此设备", DeskLinkSection.share.rawValue)
    XCTAssertNotEqual(DeskLinkSection.connect.rawValue, "本机状态")
}
```

这一步只锁定可见任务名称；邀请解析、审批和连接状态继续由现有 bridge 测试覆盖。

- [ ] **Step 2: Run the focused test before the view changes**

Run:

```sh
cd apps/macos
swift test --arch arm64 --filter DeskLinkNavigationTests
```

Expected: PASS, confirming the page work can proceed without改变内核接口。

- [ ] **Step 3: Rework `ControllerHomeView` as the default connection workspace**

将页面顺序固定为：

```swift
VStack(alignment: .leading, spacing: 16) {
    pageHeading("连接设备", "输入连接码，开始控制另一台设备")
    ConnectView(bridge: bridge)
    savedHostsPanel
    sharingEntryPanel
    diagnosticsDisclosure
    errorPanel
}
```

具体要求：

- 将当前“控制端”状态标题改为“连接设备”，运行状态只显示“准备连接 / 连接中 / 已连接 / 需要处理”等用户文案。
- 最近设备列表保留在连接首页，并将按钮统一成“连接设备”。
- `DiagnosticsView(bridge:)` 改为关闭状态的 `DisclosureGroup("连接诊断")`，不再直接占据首页主卡片。
- 控制端校验 key 保持现有折叠区域，默认关闭。
- 增加“允许别人连接此设备”次级卡片，通过父级 `DeskLinkSection.share` 进入共享页；该入口只能导航，不直接启动主机，避免误触启动共享。

- [ ] **Step 4: Add manual connection-code input without changing the bridge**

在 `ConnectView.swift` 增加本地 `@State private var inviteDraft = ""` 和 `@State private var manualEntryVisible = false`。保留粘贴板读取，但动作变为：

```swift
Button("粘贴连接码") {
    let pasted = NSPasteboard.general.string(forType: .string)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    inviteDraft = pasted
    if !pasted.isEmpty {
        bridge.connect(invite: pasted)
    }
}
.buttonStyle(DeskLinkPrimaryButtonStyle())

Button("手动输入连接码") {
    manualEntryVisible.toggle()
}
.buttonStyle(DeskLinkSecondaryButtonStyle())

if manualEntryVisible {
    TextEditor(text: $inviteDraft)
        .font(.system(size: 12, design: .monospaced))
        .frame(minHeight: 84)
    Button("开始连接") {
        bridge.connect(invite: inviteDraft.trimmingCharacters(in: .whitespacesAndNewlines))
    }
    .buttonStyle(DeskLinkPrimaryButtonStyle())
    .disabled(inviteDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
}
```

错误提示继续使用 `ControllerBridge.userFacingError`，不把内部 relay key、认证材料或 stream ID写进首页。

- [ ] **Step 5: Rework `HostHomeView` as a secondary sharing/settings surface**

将 `.connection` 页面标题和说明改为“共享此设备”“需要让别人控制这台 Mac 时，先完成权限检查并生成连接码”。保留现有 Screen Recording、Accessibility、创建邀请、审批和停止共享动作。

将 `.overview` 作为“设置 / 诊断”承载页：保留权限、审批设备和错误详情，但将当前“本机运行状态”指标包进关闭的 `DisclosureGroup("运行指标")`，默认只显示一行摘要。指标内容和 `HostMetrics` 绑定不变。

将 `.devices` 页面保留为已批准设备管理页，标题使用“已批准设备”，撤销动作继续调用 `bridge.revoke`，不改变 Keychain 数据行为。

- [ ] **Step 6: Add the status popover details**

在 `DeskLinkHostStatus.swift` 增加 `DeskLinkHostStatusPopover`，内容顺序固定为：

```swift
VStack(alignment: .leading, spacing: 12) {
    Text("本机共享")
    Label(status.title, systemImage: status.systemImage)
    Text(status.detail)
    Divider()
    Button("打开设置 / 诊断") { openSettings() }
    Button("共享此设备") { openSharing() }
}
```

按钮通过闭包回到 `DeskLinkApp` 修改 `section`，不在 popover 中复制任何 bridge 逻辑。

- [ ] **Step 7: Build and run all macOS tests for the UI slice**

Run:

```sh
cd apps/macos
swift test --arch arm64
```

Expected: all existing bridge, media, permission and new navigation tests pass. Commit:

```sh
git add apps/macos/Sources/DeskLinkApp/Views/ControllerHomeView.swift \
  apps/macos/Sources/DeskLinkApp/Views/ConnectView.swift \
  apps/macos/Sources/DeskLinkApp/Views/HostHomeView.swift \
  apps/macos/Sources/DeskLinkApp/Views/DeskLinkHostStatus.swift \
  apps/macos/Tests/DeskLinkAppTests/DeskLinkNavigationTests.swift
git commit -m "feat(macos): simplify connect and share flows"
```

---

### Task 3: 同步 Windows 页面信息架构，不触碰 Windows 内核

**Files:**
- Modify: `apps/windows-ui/src/navigation.ts`
- Modify: `apps/windows-ui/src/navigation.test.ts`
- Modify: `apps/windows-ui/src/main.ts`
- Modify: `apps/windows-ui/src/controller.ts`
- Modify: `apps/windows-ui/src/styles.css`

**Interfaces:**
- Consumes: 现有 `HostSnapshot`、`ControllerSnapshot`、Tauri API 和 `bindControllerInteractions`。
- Produces: Windows 默认 `controller` 页面、统一导航项、可点击本机状态摘要；不修改 `apps/windows-ui/src-tauri/**`、`api.ts` 或 Rust 运行时接口。

- [ ] **Step 1: Add failing tests for the cross-platform navigation contract**

在 `navigation.ts` 增加纯数据模型：

```ts
export type DeskLinkView = "overview" | "controller" | "connection" | "devices" | "pairing" | "fixedAccess";

export const DESKTOP_NAV_ITEMS: ReadonlyArray<{ id: DeskLinkView; label: string }> = [
  { id: "controller", label: "连接设备" },
  { id: "connection", label: "共享此设备" },
  { id: "devices", label: "已批准设备" },
  { id: "overview", label: "设置 / 诊断" },
];

export function navigationViewFor(view: DeskLinkView): DeskLinkView {
  if (view === "pairing") return "connection";
  if (view === "fixedAccess") return "overview";
  return view;
}
```

先在 `navigation.test.ts` 写测试：

```ts
test("uses remote tasks as the shared desktop navigation", () => {
  expect(DESKTOP_NAV_ITEMS.map((item) => item.label)).toEqual([
    "连接设备",
    "共享此设备",
    "已批准设备",
    "设置 / 诊断",
  ]);
});

test("keeps pairing and fixed access as secondary pages", () => {
  expect(navigationViewFor("pairing")).toBe("connection");
  expect(navigationViewFor("fixedAccess")).toBe("overview");
});
```

- [ ] **Step 2: Run the focused Windows test and verify it fails**

Run:

```sh
cd apps/windows-ui
bun test src/navigation.test.ts
```

Expected: FAIL because the shared navigation exports do not exist yet.

- [ ] **Step 3: Implement the shared navigation model**

把 `main.ts` 的本地 `View` 类型替换为从 `navigation.ts` 导入的 `DeskLinkView`，并改为：

```ts
let activeView: DeskLinkView = "controller";
let renderedView: DeskLinkView | null = null;
```

`renderNavigation()` 直接使用 `DESKTOP_NAV_ITEMS`，`activeNavigationView` 使用 `navigationViewFor(activeView)`。低频 `fixedAccess` 不再出现在主导航，但继续保留现有按钮 `data-open-fixed-access` 作为设置页入口。

- [ ] **Step 4: Move the Windows local status page behind settings**

保留现有 `renderOverview(state)` 的主机设置、权限、批准设备和诊断能力，但在 `renderCurrentView()` 中把它作为“设置 / 诊断”内容，并将页面标题、返回按钮和 aria 文案从“本机状态”改为“设置 / 诊断”。默认启动的 `controller` 页面不得调用或嵌入 `renderOverview`。

将以下旧文案全部替换：

```text
本机状态       -> 设置 / 诊断
控制另一台     -> 连接设备
本机连接       -> 共享此设备
返回本机状态   -> 返回设置 / 诊断
```

保留旧内部 view ID，减少事件绑定和 Tauri API 的改动面；用户可见文案统一即可。

- [ ] **Step 5: Add the Windows top-right host status chip**

在 `main.ts` 增加纯渲染函数：

```ts
function renderHostStatusChip(state: HostSnapshot): string {
  const attention = Boolean(state.connectionError || state.trustedError || state.fixedPasswordError);
  const title = attention
    ? "需要处理"
    : state.connection
      ? "本机可被连接"
      : "未开启共享";
  return `<button class="host-status-chip host-status-chip--${attention ? "attention" : "quiet"}" type="button" data-open-overview aria-label="${title}，打开设置 / 诊断">${title}</button>`;
}
```

在 `renderHeader()` 中将原本固定的“Windows 保护已启用”文本换成该摘要；DPAPI/保护状态仍保留在设置页的诊断内容中，不能删除或改变 API 读取。

- [ ] **Step 6: Synchronize the Windows controller copy and layout**

在 `controller.ts` 将控制端标题改为：

```html
<h1>连接设备</h1>
<p>粘贴连接码，开始控制另一台电脑。</p>
```

保留现有设备 ID、临时密码、邀请解析、连接、重连、视频解码、输入注入和断开逻辑。将连接面板和最近设备列表调整为同一层级的“连接设备”工作台，不能把本机状态、relay 诊断或 stream ID插入默认页面。

- [ ] **Step 7: Add the shared workspace styles without changing the design tokens**

在 `styles.css` 中新增以下选择器，复用现有 CSS 变量和按钮样式：

```css
.host-status-chip {
  border: 0;
  background: transparent;
  color: var(--muted-ink);
  font: inherit;
  cursor: pointer;
}

.host-status-chip--attention {
  color: var(--warning);
}

.connection-workspace {
  max-width: 920px;
  margin: 0 auto;
}

.share-device-card {
  border: 1px solid var(--border);
  background: var(--subtle-surface);
  border-radius: var(--radius-md);
}

.diagnostic-disclosure {
  border-top: 1px solid var(--border);
}
```

如果当前 CSS 变量名称不同，使用现有等价变量，不新增第二套颜色值。不得增加阴影或动画。

- [ ] **Step 8: Run Windows UI tests and build**

Run:

```sh
cd apps/windows-ui
bun test
bun run build
```

Expected: all tests pass and Vite completes without TypeScript errors. Commit：

```sh
git add apps/windows-ui/src/navigation.ts \
  apps/windows-ui/src/navigation.test.ts \
  apps/windows-ui/src/main.ts \
  apps/windows-ui/src/controller.ts \
  apps/windows-ui/src/styles.css
git commit -m "feat(windows-ui): align remote connection workspace"
```

---

### Task 4: 更新跨平台设计文档与使用说明

**Files:**
- Modify: `DESIGN.md`
- Modify: `README.md`

**Interfaces:**
- Consumes: 已提交设计规格 `docs/superpowers/specs/2026-07-18-unified-remote-connection-workspace-design.md` 和已实现的 macOS/Windows 页面语义。
- Produces: 未来实现者可直接遵循的跨平台页面契约，不新增运行时行为。

- [ ] **Step 1: Update the design system language**

在 `DESIGN.md` 中将“status-first hierarchy”和“四 section navigation”改为“remote-task-first hierarchy”，明确以下优先级：

```text
连接设备 -> 最近设备 -> 共享此设备 -> 已批准设备 -> 设置 / 诊断
```

补充 iOS 约束：iOS 是控制端，默认进入“连接设备”，使用底部导航或 sheet；不能把“共享此设备”渲染成可执行的 iOS 被控入口。

- [ ] **Step 2: Update the README macOS flow**

把 README 中的 macOS 页面名称更新为“连接设备”“共享此设备”“已批准设备”“设置 / 诊断”，并明确：当前仓库没有 `apps/ios` 客户端工程，因此 iOS 本轮只有统一页面契约，没有可执行产物。

- [ ] **Step 3: Check documentation references and commit**

Run:

```sh
rg -n "本机状态|控制另一台|本机连接|四.*导航|status-first" DESIGN.md README.md docs/superpowers/specs/2026-07-18-unified-remote-connection-workspace-design.md
```

Expected: only historical/spec context remains; active design and README instructions use the new remote-task-first language. Commit:

```sh
git add DESIGN.md README.md
git commit -m "docs: align cross-platform remote UX guidance"
```

---

### Task 5: 完成验证并检查实现边界

**Files:**
- Modify: no source files; only generated verification output is allowed if an existing script creates it.

**Interfaces:**
- Consumes: Task 1–4 的页面实现和现有 macOS/Windows 测试命令。
- Produces: 可复现的 macOS 内核回归结果、Windows 页面构建结果和边界审计结果。

- [ ] **Step 1: Run the complete macOS Swift test suite**

Run:

```sh
cd apps/macos
swift test --arch arm64
```

Expected: existing bridge, encoder/decoder, input, permission, geometry and navigation tests all PASS.

- [ ] **Step 2: Run the macOS runtime verification script**

Run from repository root:

```sh
./scripts/verify-macos-runtime.sh
```

Expected: Rust FFI、local-relay fake-media 端到端测试和 Swift arm64 测试全部通过；若因本机缺少签名或系统权限失败，记录准确失败阶段，不修改测试以绕过失败。

- [ ] **Step 3: Run Windows UI tests and build on the macOS host**

Run:

```sh
cd apps/windows-ui
bun test
bun run build
```

Expected: Windows UI TypeScript tests and Vite build pass；不执行 Windows-only Tauri release、DXGI、Win32 input 或 PE checks，因为当前环境是 macOS。

- [ ] **Step 4: Audit the implementation boundary**

Run:

```sh
git diff --name-only HEAD~4..HEAD
git diff -- apps/windows-ui/src-tauri apps/windows-ui/src/api.ts
rg -n "视频数据包|关键帧请求|stream ID|本机运行状态" apps/macos/Sources/DeskLinkApp apps/windows-ui/src
```

Expected:

- Windows diff does not contain `apps/windows-ui/src-tauri/**` or `apps/windows-ui/src/api.ts` changes.
- Runtime metrics and stream IDs only appear in settings/diagnostic/session contexts, not the default connection workspace.
- No iOS implementation is falsely claimed.

- [ ] **Step 5: Commit only if verification artifacts require a tracked change**

If verification produces no intended source or documentation changes, do not create an empty commit. Record the commands and outcomes in the final handoff. If a tracked test fixture or documentation correction is required by an observed failure, commit it separately with a message naming the verified issue.

---

## Self-review checklist

- [x] Spec coverage: default connection page, secondary sharing flow, low-priority diagnostics, active session focus, shared desktop wording, iOS controller-only boundary and macOS-only runtime verification each map to a task.
- [x] No Windows kernel work: Task 3 explicitly limits changes to `apps/windows-ui/src/**` and leaves `src-tauri/**` and `api.ts` untouched.
- [x] No iOS false implementation: Task 4 documents the future iOS contract and Task 5 explicitly avoids claiming an iOS build.
- [x] Type consistency: macOS uses `DeskLinkSection` and `DeskLinkHostStatus`; Windows uses `DeskLinkView`, `DESKTOP_NAV_ITEMS` and `navigationViewFor` consistently across implementation and tests.
- [x] Verification is proportionate to the environment: macOS runtime and Swift tests are required; Windows UI test/build is run on macOS; Windows-only runtime checks are excluded.

