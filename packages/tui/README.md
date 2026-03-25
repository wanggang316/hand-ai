# hand-tui

Terminal UI component library with differential rendering, theming, and a set of built-in components.

## Features

- Component model: `Component`, `Focusable`, `Container`
- Differential rendering: `DiffRenderer` minimizes terminal redraws
- Theme system: `Theme`, `Style`, `Color` with dark/light presets
- Key parsing: Kitty keyboard protocol, standard CSI, SS3
- 12 built-in components
- ANSI-aware text utilities (width, truncation, wrapping)

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

All components implement this trait:

```rust
trait Component {
    fn render(&self, width: usize) -> Vec<String>;
    fn handle_input(&mut self, data: &str) -> HandleResult;
    fn invalidate(&mut self);
    fn wants_key_release(&self) -> bool;
}
```

### `Focusable`

Components that accept keyboard input:

```rust
trait Focusable: Component {
    fn focused(&self) -> bool;
    fn set_focused(&mut self, focused: bool);
    fn cursor_position(&self) -> Option<(usize, usize)>;
}
```

### `Container`

Manages child components:

```rust
let mut root = Container::new();
root.add_child(Box::new(text));
root.remove_child(0);
root.clear();
```

## Built-in Components

| Component | Description |
|-----------|-------------|
| `TextComponent` | Static/dynamic text with optional padding |
| `TruncatedTextComponent` | Text that truncates to width with ellipsis |
| `InputComponent` | Single-line input with history, placeholder, prefix |
| `EditorComponent` | Multi-line editor with undo/redo, viewport scrolling |
| `MarkdownComponent` | Markdown rendering (headings, code blocks, lists, bold, italic) |
| `LoaderComponent` | Animated spinner with configurable frames and colors |
| `SelectListComponent` | Navigable list with selection, home/end support |
| `BoxComponent` | Wrapper with padding and optional background color |
| `SpacerComponent` | Empty space of configurable height |
| `StatusBarComponent` | Left/center/right sections with configurable style |
| `ProgressBarComponent` | Horizontal progress bar with label and percentage |
| `ToastManager` | Notification stack with Info/Success/Warning/Error levels |
| `AutocompleteComponent` | Suggestion dropdown with navigation and scrolling |

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
cargo test   # 147 tests
```

## License

MIT

## See Also

- [hand-coding-agent](../coding-agent) — Uses hand-tui for its terminal interface
