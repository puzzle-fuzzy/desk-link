# DeskLink Windows UI 样式模板

这套模板从产品化远程桌面工具提取了可复用的视觉规则，服务于“连接设备”这个唯一首要任务。它不是新的组件库，而是围绕现有 HTML 类名的稳定设计约束；低频的共享、批准设备、设置 / 诊断和关于入口统一放入“更多”菜单。

## 视觉方向

- **基调**：白色工作区、低对比中性灰、清晰的蓝色主动作，接近成熟的 Windows 产品工具。
- **信息层级**：产品标题 → 连接设备 → 设备 ID / 访问密码 → 主按钮 → 已保存连接。
- **版式**：内容居中，主连接卡片与已保存连接并列；宽屏保持舒适最大宽度，小屏自动单列。
- **形状**：按钮、输入框、卡片统一使用 8–16px 圆角；不使用阴影，层次通过边框和背景色建立。
- **字体**：`v-sans, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif, "Apple Color Emoji", "Segoe UI Emoji", "Segoe UI Symbol"`；设备 ID / 密码保留等宽字体以便核对。
- **交互**：蓝色只代表主动作、当前页或选中项；悬停只改变边界和背景，不移动布局。

## 设计令牌

```css
:root {
  --background: #f7f9fc;
  --surface: #ffffff;
  --surface-subtle: #f4f7fb;
  --surface-quiet: #edf2f7;
  --ink: #172033;
  --ink-secondary: #45536a;
  --ink-muted: #718096;
  --border: #e3e8f0;
  --border-strong: #c8d1df;
  --primary: #1677ff;
  --primary-hover: #0d68e8;
  --primary-pressed: #095acb;
  --on-primary: #ffffff;
  --radius-sm: 8px;
  --radius-md: 12px;
  --radius-lg: 16px;
}
```

## 页面骨架

```html
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
```

## 组件规则

| 组件 | 规则 |
| --- | --- |
| 主按钮 | `--primary` 填充、白色文字、10px 圆角、44–48px 高 |
| 次按钮 | 白色底、`--border-strong` 边框，悬停变浅灰 / 蓝色 |
| 连接标题 | 30–42px、700 字重，宽屏不使用过大的展示字 |
| 输入框 | 1px 边框、10px 圆角、ID/密码使用等宽字体、56px 高 |
| 卡片 | 1px 边框、12–16px 圆角、轻量阴影，避免网格装饰 |
| 错误提示 | 保留原有语义色与可访问性，不用动画推动布局 |

## 交互与无障碍约束

- 保留现有 `data-*` 选择器、键盘焦点和 `aria-*` 属性，不为了视觉重构删除功能钩子。
- 选中态必须同时有颜色和文字/边线差异，不能只靠颜色传达状态。
- `prefers-reduced-motion` 和 `forced-colors` 下继续可用。
- 响应式断点：宽度低于 860px 时两列变单列，低于 640px 时标题和操作垂直排列。
