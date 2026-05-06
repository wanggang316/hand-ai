# hand-tui

Terminal UI component library with differential rendering, theming, and a set of built-in components.

## Features

- Component model: `Component`, `Focusable`, `Container`
- Differential rendering: `DiffRenderer` minimizes terminal redraws
- Theme system: `Theme`, `Style`, `Color` with dark/light presets
- Key parsing: Kitty keyboard protocol, standard CSI, SS3
- 16 built-in components (see table below)
- ANSI-aware text utilities (width, truncation, wrapping)
- Terminal image rendering (Kitty / iTerm / Sixel) and resize handling
- Pluggable keybindings, raw stdin buffering, structured errors

## Installation

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

## Core Traits

### `Component`

All components implement this trait. `hide` / `show` / `set_hidden` /
`is_hidden` provide a unified visibility model that mirrors the upstream
`pi-tui` TS surface; default impls treat the component as always visible
and the visibility hooks as no-ops.

```rust
trait Component: Send {
    fn render(&self, width: u16) -> Vec<String>;
    fn handle_input(&mut self, event: &InputEvent) -> HandleResult { /* default: Ignored */ }
    fn invalidate(&mut self) {}
    fn wants_key_release(&self) -> bool { false }

    fn hide(&mut self) { self.set_hidden(true); }
    fn show(&mut self) { self.set_hidden(false); }
    fn set_hidden(&mut self, _hidden: bool) {}
    fn is_hidden(&self) -> bool { false }
}
```

### `Focusable`

Components that accept keyboard input. `focus` / `unfocus` / `is_focused`
are convenience wrappers over the `set_focused` / `focused` pair.

```rust
trait Focusable: Component {
    fn focused(&self) -> bool;
    fn set_focused(&mut self, focused: bool);
    fn cursor_position(&self) -> Option<(u16, u16)>;

    fn focus(&mut self) { self.set_focused(true); }
    fn unfocus(&mut self) { self.set_focused(false); }
    fn is_focused(&self) -> bool { self.focused() }
}
```

### `Container`

Manages child components by stable `ComponentId`:

```rust
let mut root = Container::new();
let id = root.add_child_with_id(Box::new(text));
root.remove_child_by_id(id);
root.clear();
```

## Built-in Components

| Component | Description |
|-----------|-------------|
| `TextComponent` | Static/dynamic text with optional padding |
| `TruncatedTextComponent` | Text that truncates to width with ellipsis |
| `InputComponent` | Single-line input with history, placeholder, prefix |
| `EditorComponent` | Multi-line editor with undo/redo, viewport scrolling, kill-ring |
| `MarkdownComponent` | Markdown rendering (headings, code blocks, lists, bold, italic) |
| `LoaderComponent` | Animated spinner with configurable frames and colors |
| `SelectListComponent` | Navigable list with selection, home/end support |
| `BoxComponent` | Wrapper with padding and optional background color |
| `SpacerComponent` | Empty space of configurable height |
| `StatusBarComponent` | Left/center/right sections with configurable style |
| `ProgressBarComponent` | Horizontal progress bar with label and percentage |
| `ToastComponent` | Notification stack with Info/Success/Warning/Error levels |
| `AutocompleteComponent` | Suggestion dropdown with navigation and scrolling |
| `CancellableLoaderComponent` | Loader with cancel keybinding and timeout hooks |
| `ImageComponent` | Inline image rendering via Kitty / iTerm / Sixel protocols |
| `SettingsListComponent` | Settings rows (toggles, choices, text fields) |

### InputComponent

```rust
let mut input = InputComponent::new();
input.set_placeholder("Type a message...");
input.set_prefix("> ");

// History support
input.push_history("previous command");

// Get current text
let text = input.text();
```

### EditorComponent

```rust
let mut editor = EditorComponent::new();
editor.set_text("Initial content");
editor.set_show_border(true);

// Undo/redo
editor.undo();
editor.redo();
```

### MarkdownComponent

```rust
let md = MarkdownComponent::new("# Hello\n\nThis is **bold** and `code`.");
let lines = md.render(80);
```

### SelectListComponent

```rust
let items = vec!["Option A", "Option B", "Option C"];
let mut list = SelectListComponent::new(items);
list.next();  // Navigate down
let selected = list.selected_item();
```

## Module Overview

Top-level modules in addition to the component layer:

- `terminal_image` — encode images for Kitty, iTerm2, and Sixel terminals; size detection and protocol negotiation.
- `keybindings` — `KeybindingsManager` with named actions, default sets, and per-context overrides.
- `stdin_buffer` — non-blocking buffered reader over raw stdin with chunk reassembly for paste and bracketed-paste sequences.
- `resize` — SIGWINCH-driven terminal size watcher exposing a polled API for re-layout.
- `error` — `TuiError` enum used by `Tui::run` and terminal I/O paths.

## Theme System

```rust
use hand_tui::theme::{Theme, Style, Color, NamedColor};

// Built-in themes
let dark = Theme::dark();
let light = Theme::light();

// Custom styles
let style = Style::new()
    .fg(Color::Named(NamedColor::Green))
    .bold()
    .apply("styled text");

// Colors: Named, Index(u8), Rgb(u8,u8,u8), Hex(String)
let color = Color::Hex("#ff6600".to_string());
```

## Differential Rendering

`DiffRenderer` compares previous and current render output, only sending changes to the terminal:

```rust
use hand_tui::DiffRenderer;

let mut renderer = DiffRenderer::new();

// First render is always full
let output = renderer.diff(&new_lines);

// Subsequent renders only output changed lines
let output = renderer.diff(&updated_lines);
```

## Key Parsing

```rust
use hand_tui::{parse_key, Key, KeyName, KeyModifiers};

let key = parse_key("\x1b[A");  // Up arrow
assert!(key.name == KeyName::Up);

let key = parse_key("\x03");    // Ctrl+C
assert!(key.modifiers.ctrl);
```

Supports: standard CSI sequences, SS3 function keys, Kitty keyboard protocol, Unicode input.

## Text Utilities

```rust
use hand_tui::utils::*;

// ANSI-aware visible width
let width = visible_width("\x1b[31mhello\x1b[0m");  // 5

// Truncate to width
let truncated = truncate_to_width("long text here", 8);  // "long ..."

// Wrap text at width
let lines = wrap_text("long paragraph...", 40);

// Strip ANSI codes
let plain = strip_ansi("\x1b[1mbold\x1b[0m");  // "bold"
```

## Terminal Abstraction

`Terminal` trait isolates terminal I/O for testability:

```rust
use hand_tui::{Terminal, TestTerminal, TerminalCapabilities};

// For testing
let mut term = TestTerminal::new(80, 24);

// Check capabilities
let caps = TerminalCapabilities::default();
```

## Development

```bash
cd packages/tui
cargo check
cargo test
```

## Migration / Known Limitations

This is the first release of `hand-tui`, ported from the TypeScript
`pi-tui` library. The following behavioral divergences and limitations
are known; downstream consumers should be aware of them.

- **`EditorComponent::yank_pop` perturbs the redo stack.** Cycling the
  kill-ring with `M-y` clears any in-flight redo history that the
  TS port preserves. See `packages/tui/src/components/editor.rs`
  (`yank_pop` impl).
- **`SelectListComponent::set_filter` / `set_selected_index` do not
  fire `on_selection_change`** for programmatic changes — only
  user-driven navigation triggers the callback. See
  `packages/tui/src/components/select_list.rs`.
- **`KeybindingsManager::unset` permanently disables a binding** rather
  than restoring the framework default, which is a divergence from
  pi-tui. To restore a default, re-register it explicitly. See
  `packages/tui/src/keybindings.rs`.
- **`Tui::run` is single-shot.** Calling it twice on the same `Tui`
  without a manual stdin-reader teardown leaks the background reader
  task. Construct a new `Tui` per session for now. See
  `packages/tui/src/tui.rs` (`Tui::run`).
- **`ProcessTerminal::Drop` does not run on `panic = "abort"` profiles.**
  Terminal restoration relies on stack unwinding; binaries built with
  `panic = "abort"` may exit with the terminal in raw / alt-screen mode.
  Install a panic hook if you need guaranteed restoration. See
  `packages/tui/src/terminal.rs`.
- **Rainbow flag emoji (🏳️‍🌈) measures display width 1 in this port**
  versus 2 with pi-tui's `Intl.Segmenter`. Other ZWJ sequences may
  differ similarly because `unicode-width` doesn't fully model
  grapheme cluster widths. See `packages/tui/src/utils.rs`
  (`visible_width`).
- **`parse_key` returns `Key` (not `Option<Key>`)** for backward
  compatibility with components that ship today. Unrecognized
  sequences map to a synthetic `Key { name: KeyName::Unknown, .. }`.
  This may tighten to `Option<Key>` in a future minor version.

For a full list of porting findings, see
`docs/exec-plans/hand-tui-port-from-pi-tui.md`.

## License

MIT

## See Also

- [hand-coding-agent](../coding-agent) — Uses hand-tui for its terminal interface
