//! Core TUI engine and component model.
//!
//! Defines the Component trait, Container, Focusable, and the main Tui struct.

use crate::render::DiffRenderer;
use crate::terminal::Terminal;

/// Result of handling user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandleResult {
    /// Input was handled by the component.
    Handled,
    /// Input was not handled; pass to parent.
    Ignored,
}

/// Structured input event delivered to components.
///
/// Bridges the M1 input layer (`stdin_buffer` + `keys`) with the component model.
/// Components that previously dispatched on raw `&str` should match on `Raw`/`Paste`.
#[derive(Debug, Clone)]
pub enum InputEvent {
    /// A semantic key from the parser. Use `keys::matches_key` or
    /// `KeybindingsManager::matches` to dispatch.
    Key(crate::keys::Key),
    /// Raw bytes from stdin — useful for components that want to consume escape
    /// sequences the parser didn't classify (terminal-image protocol replies,
    /// mouse events, etc.).
    Raw(String),
    /// A synthetic paste event (multi-byte payload pre-classified by `stdin_buffer`).
    Paste(String),
    /// Terminal was resized.
    Resize { cols: u16, rows: u16 },
    /// Periodic tick — used for animations and debounced timers.
    Tick,
}

/// Wrap a `&str` payload as `InputEvent::Raw`. Convenience helper for migration
/// callsites and tests that previously passed raw escape sequences directly.
pub fn input_event_from_str(data: &str) -> InputEvent {
    InputEvent::Raw(data.to_string())
}

/// Core component trait — all UI elements implement this.
pub trait Component: Send {
    /// Render the component to a list of terminal lines.
    /// `width` is the available terminal width in columns.
    fn render(&self, width: u16) -> Vec<String>;

    /// Handle a structured input event. Returns whether the input was consumed.
    fn handle_input(&mut self, _event: &InputEvent) -> HandleResult {
        HandleResult::Ignored
    }

    /// Invalidate any cached render state.
    fn invalidate(&mut self) {}

    /// Whether this component wants key release events.
    fn wants_key_release(&self) -> bool {
        false
    }

    /// Hide this component (skipped by container rendering).
    fn hide(&mut self) {
        self.set_hidden(true);
    }

    /// Show this component (default visible state).
    fn show(&mut self) {
        self.set_hidden(false);
    }

    /// Set the hidden flag. Default impl is a no-op for components that are
    /// always visible.
    fn set_hidden(&mut self, _hidden: bool) {}

    /// Whether this component is currently hidden.
    fn is_hidden(&self) -> bool {
        false
    }
}

/// A focusable component that supports cursor positioning.
pub trait Focusable: Component {
    /// Whether this component currently has focus.
    fn focused(&self) -> bool;

    /// Set focus state.
    fn set_focused(&mut self, focused: bool);

    /// Cursor position relative to the component (col, row), if visible.
    fn cursor_position(&self) -> Option<(u16, u16)>;

    /// Convenience: focus this component.
    fn focus(&mut self) {
        self.set_focused(true);
    }

    /// Convenience: unfocus this component.
    fn unfocus(&mut self) {
        self.set_focused(false);
    }

    /// Alias for `focused()` to mirror the pi-tui TS API.
    fn is_focused(&self) -> bool {
        self.focused()
    }
}

/// Container that manages child components.
pub struct Container {
    children: Vec<Box<dyn Component>>,
    hidden: bool,
}

impl Container {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            hidden: false,
        }
    }

    pub fn add_child(&mut self, child: Box<dyn Component>) {
        self.children.push(child);
    }

    pub fn remove_child(&mut self, index: usize) -> Option<Box<dyn Component>> {
        if index < self.children.len() {
            Some(self.children.remove(index))
        } else {
            None
        }
    }

    pub fn children(&self) -> &[Box<dyn Component>] {
        &self.children
    }

    pub fn children_mut(&mut self) -> &mut [Box<dyn Component>] {
        &mut self.children
    }

    pub fn child_count(&self) -> usize {
        self.children.len()
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }
}

impl Default for Container {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for Container {
    fn render(&self, width: u16) -> Vec<String> {
        let mut lines = Vec::new();
        for child in &self.children {
            if child.is_hidden() {
                continue;
            }
            lines.extend(child.render(width));
        }
        lines
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        // Try each child in reverse order (last child = topmost). Skip hidden.
        for child in self.children.iter_mut().rev() {
            if child.is_hidden() {
                continue;
            }
            if child.handle_input(event) == HandleResult::Handled {
                return HandleResult::Handled;
            }
        }
        HandleResult::Ignored
    }

    fn invalidate(&mut self) {
        for child in &mut self.children {
            child.invalidate();
        }
    }

    fn set_hidden(&mut self, hidden: bool) {
        self.hidden = hidden;
    }

    fn is_hidden(&self) -> bool {
        self.hidden
    }
}

/// Main TUI engine that manages terminal rendering.
pub struct Tui {
    terminal: Box<dyn Terminal>,
    root: Container,
    renderer: DiffRenderer,
    running: bool,
}

impl Tui {
    /// Create a new TUI with the given terminal backend.
    pub fn new(terminal: Box<dyn Terminal>) -> Self {
        Self {
            terminal,
            root: Container::new(),
            renderer: DiffRenderer::new(),
            running: false,
        }
    }

    /// Access the root container.
    pub fn root(&self) -> &Container {
        &self.root
    }

    /// Access the root container mutably.
    pub fn root_mut(&mut self) -> &mut Container {
        &mut self.root
    }

    /// Perform a render cycle: render components and write changes to terminal.
    pub fn render(&mut self) {
        let width = self.terminal.columns();
        let new_lines = self.root.render(width);
        let commands = self.renderer.diff(&new_lines);
        if !commands.is_empty() {
            self.terminal.write(&commands);
        }
    }

    /// Force a full re-render (bypass differential rendering).
    pub fn force_render(&mut self) {
        self.renderer.reset();
        self.render();
    }

    /// Whether the TUI is currently running.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Stop the TUI.
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Get terminal dimensions.
    pub fn size(&self) -> (u16, u16) {
        (self.terminal.columns(), self.terminal.rows())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestComponent {
        lines: Vec<String>,
    }

    impl TestComponent {
        fn new(lines: Vec<&str>) -> Self {
            Self {
                lines: lines.into_iter().map(String::from).collect(),
            }
        }
    }

    impl Component for TestComponent {
        fn render(&self, _width: u16) -> Vec<String> {
            self.lines.clone()
        }
    }

    #[test]
    fn test_container_add_remove() {
        let mut container = Container::new();
        assert_eq!(container.child_count(), 0);

        container.add_child(Box::new(TestComponent::new(vec!["hello"])));
        assert_eq!(container.child_count(), 1);

        container.remove_child(0);
        assert_eq!(container.child_count(), 0);
    }

    #[test]
    fn test_container_render_concatenates() {
        let mut container = Container::new();
        container.add_child(Box::new(TestComponent::new(vec!["line1"])));
        container.add_child(Box::new(TestComponent::new(vec!["line2", "line3"])));

        let lines = container.render(80);
        assert_eq!(lines, vec!["line1", "line2", "line3"]);
    }

    #[test]
    fn test_container_clear() {
        let mut container = Container::new();
        container.add_child(Box::new(TestComponent::new(vec!["x"])));
        container.add_child(Box::new(TestComponent::new(vec!["y"])));
        assert_eq!(container.child_count(), 2);
        container.clear();
        assert_eq!(container.child_count(), 0);
    }

    #[test]
    fn test_handle_result_default_ignored() {
        let mut comp = TestComponent::new(vec!["test"]);
        assert_eq!(
            comp.handle_input(&InputEvent::Raw("x".into())),
            HandleResult::Ignored
        );
    }

    #[test]
    fn test_input_event_from_str() {
        match input_event_from_str("\x1b[A") {
            InputEvent::Raw(s) => assert_eq!(s, "\x1b[A"),
            _ => panic!("expected Raw"),
        }
    }

    #[test]
    fn test_container_skips_hidden_children() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        #[derive(Clone)]
        struct Counter(Arc<AtomicU32>);
        impl Counter {
            fn new() -> Self {
                Self(Arc::new(AtomicU32::new(0)))
            }
            fn bump(&self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
            fn get(&self) -> u32 {
                self.0.load(Ordering::Relaxed)
            }
        }

        struct HideableComponent {
            line: String,
            hidden: bool,
            received: Counter,
        }
        impl Component for HideableComponent {
            fn render(&self, _w: u16) -> Vec<String> {
                vec![self.line.clone()]
            }
            fn handle_input(&mut self, _e: &InputEvent) -> HandleResult {
                self.received.bump();
                HandleResult::Ignored
            }
            fn set_hidden(&mut self, hidden: bool) {
                self.hidden = hidden;
            }
            fn is_hidden(&self) -> bool {
                self.hidden
            }
        }

        let visible_count = Counter::new();
        let hidden_count = Counter::new();

        let mut container = Container::new();
        container.add_child(Box::new(HideableComponent {
            line: "visible".into(),
            hidden: false,
            received: visible_count.clone(),
        }));
        container.add_child(Box::new(HideableComponent {
            line: "hidden".into(),
            hidden: true,
            received: hidden_count.clone(),
        }));

        // Render skip
        let lines = container.render(80);
        assert_eq!(lines, vec!["visible"]);

        // Input dispatch skip
        container.handle_input(&InputEvent::Raw("x".into()));
        assert_eq!(visible_count.get(), 1, "visible child must receive input");
        assert_eq!(hidden_count.get(), 0, "hidden child must NOT receive input");
    }
}
