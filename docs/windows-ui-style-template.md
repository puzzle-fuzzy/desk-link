# DeskLink Windows UI 样式模板

这套模板从 RemoteFlow 控制台参考页提取了可复用的视觉规则，服务于“连接设备”这个唯一首要任务。它不是新的组件库，而是围绕现有 HTML 类名的稳定设计约束；共享、批准设备和设置 / 诊断统一进入“更多”菜单，关于和项目链接也保留在同一菜单。

## 视觉方向

- **基调**：RemoteFlow 系统化极简；近白色工作区、低对比中性灰、清晰的蓝色主动作。
- **信息层级**：产品标题 → 连接设备 → 设备 ID / 访问密码 → 主按钮 → 已保存连接。
- **版式**：4px 基线节奏、16px 功能间距、桌面端 32px 外边距；原生 Windows 标题栏下方使用 240px 导航栏、64px 工作区上下文栏，主连接卡片与已保存连接并列，小屏自动单列。
- **形状**：按钮、输入框、卡片统一使用 8px 圆角；普通卡片不使用阴影，层次通过边框和背景色建立。
- **字体**：`v-sans, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif, "Apple Color Emoji", "Segoe UI Emoji", "Segoe UI Symbol"`；设备 ID / 密码保留等宽字体以便核对。
- **交互**：蓝色只代表主动作、当前页或选中项；悬停只改变边界和背景，不移动布局。

## 设计令牌

```css
:root {
  --background: #f9f9ff;
  --surface: #ffffff;
  --surface-subtle: #f8fafc;
  --surface-quiet: #f0f3ff;
  --ink: #111c2d;
  --ink-secondary: #434655;
  --ink-muted: #64748b;
  --border: #e2e8f0;
  --border-strong: #c3c6d7;
  --primary: #2563eb;
  --primary-hover: #1d4ed8;
  --primary-pressed: #1e40af;
  --on-primary: #ffffff;
  --radius-sm: 4px;
  --radius-md: 8px;
  --radius-lg: 8px;
}
```

## 页面骨架

```html
<div class="app-shell">
  <aside class="app-sidebar">DeskLink / 主要功能</aside>
  <div class="app-main">
    <header class="workspace-topbar">工作区上下文与更多菜单</header>
    <main class="workspace">
  <section class="controller-stack">
    <header class="controller-heading">
      <div>
        <h1>连接设备</h1>
        <p>一句话说明用户下一步要做什么。</p>
      </div>
    </header>
    <div class="controller-connect-layout">
      <section class="controller-card controller-card--primary">输入设备 ID 和访问密码</section>
      <aside class="saved-devices-panel">已保存连接</aside>
    </div>
  </section>
    </main>
  </div>
</div>
```

## 组件规则

| 组件 | 规则 |
| --- | --- |
| 主按钮 | `#2563eb` 填充、白色文字、8px 圆角、44px 高 |
| 次按钮 | 白色底、`#c3c6d7` 边框，悬停变浅灰 / 蓝色 |
| 连接标题 | 30px、600 字重、38px 行高，宽屏不使用过大的展示字 |
| 输入框 | 1px 边框、8px 圆角、ID/密码使用等宽字体、48px 高；聚焦为 2px 蓝色边框 |
| 卡片 | 1px `#e2e8f0` 边框、8px 圆角、无阴影 |
| 错误提示 | 保留原有语义色与可访问性，不用动画推动布局 |

## 交互与无障碍约束

- 保留现有 `data-*` 选择器、键盘焦点和 `aria-*` 属性，不为了视觉重构删除功能钩子。
- 窗口控制交给 Windows 原生标题栏，不在 WebView 中重绘最小化、最大化和关闭按钮。
- 选中态必须同时有颜色和文字/边线差异，不能只靠颜色传达状态。
- `prefers-reduced-motion` 和 `forced-colors` 下继续可用。
- 响应式断点：宽度低于 860px 时两列变单列，低于 640px 时标题和操作垂直排列。
