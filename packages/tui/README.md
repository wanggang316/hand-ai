# hand-tui

Rust 终端 UI 组件库，提供组件模型、差量渲染和一组可直接复用的内建组件。

这个包聚焦于 Rust 终端场景。

## Features

- 组件化渲染模型：`Component`、`Focusable`、`Container`
- 差量渲染：`DiffRenderer`
- 终端抽象：`Terminal`、`TerminalCapabilities`
- 常用组件：输入框、编辑器、Markdown、选择列表、加载器等
- ANSI 宽度与文本换行工具

## 安装

```toml
[dependencies]
hand-tui = { path = "../tui" }
```

## Quick Start

```rust
use hand_tui::{Container, Component, TextComponent};

fn main() {
    let mut root = Container::new();
    root.add_child(Box::new(TextComponent::new("Hello from hand-tui")));

    let lines = root.render(80);
    for line in lines {
        println!("{line}");
    }
}
```

## Core API

### `Component`

所有组件都实现这个 trait：

- `render(width) -> Vec<String>`
- `handle_input(data) -> HandleResult`
- `invalidate()`
- `wants_key_release()`

### `Focusable`

可获取焦点的组件在 `Component` 之上额外实现：

- `focused()`
- `set_focused()`
- `cursor_position()`

### `Container`

用于管理子组件：

- `add_child()`
- `remove_child()`
- `children()` / `children_mut()`
- `child_count()`
- `clear()`

### `Tui`

主引擎负责协调：

- 根组件树
- 终端输出
- 差量渲染器

## 内建组件

当前导出的组件包括：

- `TextComponent`
- `TruncatedTextComponent`
- `InputComponent`
- `EditorComponent`
- `MarkdownComponent`
- `LoaderComponent`
- `SelectListComponent`
- `SpacerComponent`
- `BoxComponent`

这些组件都从 `packages/tui/src/components/` 导出，可直接复用。

## 差量渲染

`DiffRenderer` 用于减少终端重绘开销，只输出必要变更。

对于持续刷新的交互式终端应用，优先通过差量渲染而不是整屏清空重画。

## 按键与文本工具

### 按键

- `Key`
- `KeyModifiers`
- `parse_key()`

### 文本工具

- `visible_width()`
- `truncate_to_width()`
- `wrap_text()`

这些工具专门处理 ANSI 转义序列和宽字符宽度问题。

## 终端抽象

`Terminal` trait 用于隔离具体终端能力，`TerminalCapabilities` 用于描述功能支持情况。

这让 `hand-tui` 更容易做测试，也便于替换底层终端实现。

## 适用场景

- 终端聊天界面
- 终端编辑器或命令面板
- 流式输出查看器
- 轻量级 dashboard

## 开发

```bash
cd packages/tui
cargo check
cargo test
```

如果新增组件，请同步更新导出列表和 README 的组件章节。

## License

MIT
