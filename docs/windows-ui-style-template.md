# DeskLink Windows UI 样式模板

这套模板从参考图提取了可复用的视觉规则，服务于“连接设备”和“共享此设备”两个核心任务。它不是新的组件库，而是围绕现有 HTML 类名的稳定设计约束。

## 视觉方向

- **基调**：编辑部 / 纸张工作板。米白底、黑色正文、深蓝动作色。
- **信息层级**：页面编号 → 大标题 → 一句任务说明 → 分区内容。
- **版式**：细线分隔、两列网格、2px 网格间距、宽阔外边距。
- **形状**：默认不使用圆角和阴影；状态、按钮和卡片靠颜色与边界区分。
- **字体**：中文使用 Segoe UI Variable；编号、状态、设备 ID 使用 Cascadia Mono。
- **交互**：蓝色只代表主动作、当前页或选中项；悬停只改变边界和颜色，不移动布局。

## 设计令牌

```css
:root {
  --background: #f3f2ee;
  --surface: #fffefa;
  --surface-subtle: #efeee9;
  --surface-quiet: #e4e3de;
  --ink: #12130f;
  --ink-secondary: #343630;
  --ink-muted: #73756e;
  --border: #d4d3cd;
  --border-strong: #969890;
  --primary: #0c38b5;
  --primary-hover: #082d98;
  --primary-pressed: #062578;
  --on-primary: #fffefa;
  --radius-sm: 0;
  --radius-md: 0;
  --radius-lg: 0;
}
```

## 页面骨架

```html
<main class="workspace">
  <section class="page-layout">
    <header class="page-heading">
      <div>
        <span class="editorial-kicker">01 / REMOTE CONTROL</span>
        <h1>连接设备</h1>
        <p>一句话说明用户下一步要做什么。</p>
      </div>
    </header>
    <div class="two-column-board">
      <section class="board-cell board-cell--primary">核心动作</section>
      <aside class="board-cell">最近设备 / 辅助信息</aside>
    </div>
  </section>
</main>
```

## 组件规则

| 组件 | 规则 |
| --- | --- |
| 主按钮 | `--primary` 填充、黑色或白色文字、0 圆角、40–48px 高 |
| 次按钮 | 白色底、`--border-strong` 边框，悬停变蓝 |
| 编号 | `Cascadia Mono`、10–11px、蓝色、字距 0.14–0.18em |
| 输入框 | 1px 边框、0 圆角、ID/密码使用等宽字体 |
| 网格卡片 | `gap: 2px`，背景使用 `--border`，格子使用 `--surface-subtle` |
| 错误提示 | 保留原有语义色与可访问性，不用动画推动布局 |

## 交互与无障碍约束

- 保留现有 `data-*` 选择器、键盘焦点和 `aria-*` 属性，不为了视觉重构删除功能钩子。
- 选中态必须同时有颜色和文字/边线差异，不能只靠颜色传达状态。
- `prefers-reduced-motion` 和 `forced-colors` 下继续可用。
- 响应式断点：宽度低于 860px 时两列变单列，低于 640px 时标题和操作垂直排列。
