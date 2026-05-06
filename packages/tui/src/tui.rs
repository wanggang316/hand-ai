//! Core TUI engine and component model.
//!
//! Defines the [`Component`] trait, [`Container`], [`Focusable`], and the main
//! [`Tui`] runtime. `Tui::run` drives an async loop that:
//!
//! 1. Reads stdin via [`StdinBuffer`](crate::stdin_buffer::StdinBuffer).
//! 2. Parses sequences via [`keys::parse_key`](crate::keys::parse_key) into
//!    [`InputEvent::Key`] / [`InputEvent::Raw`].
//! 3. Runs registered [`InputListener`]s (which may consume the event or
//!    rewrite its payload).
//! 4. Dispatches to the focused child first, then falls back to the root
//!    container for tree-wide handling.
//! 5. Coalesces render requests on a fixed tick so bursty input doesn't drown
//!    the terminal.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;
use tokio::time;

use crate::error::{TuiError, TuiResult};
use crate::render::DiffRenderer;
use crate::stdin_buffer::{StdinBuffer, StdinBufferEvent};
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

/// Stable identifier for a child of a [`Container`]. Returned by
/// [`Container::add_child_with_id`] so callers can later focus, look up, or
/// remove a specific child without juggling positional indices that shift on
/// insertion/removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ComponentId(u64);

impl ComponentId {
    /// Return the underlying numeric id (mostly useful in debugging).
    pub fn raw(self) -> u64 {
        self.0
    }
}

/// Container that manages child components.
pub struct Container {
    children: Vec<(ComponentId, Box<dyn Component>)>,
    next_id: u64,
    hidden: bool,
}

impl Container {
    pub fn new() -> Self {
        Self {
            children: Vec::new(),
            next_id: 0,
            hidden: false,
        }
    }

    /// Add a child without caring about its id (legacy convenience).
    pub fn add_child(&mut self, child: Box<dyn Component>) {
        let _ = self.add_child_with_id(child);
    }

    /// Add a child and return a stable [`ComponentId`] for later lookup.
    pub fn add_child_with_id(&mut self, child: Box<dyn Component>) -> ComponentId {
        let id = ComponentId(self.next_id);
        self.next_id += 1;
        self.children.push((id, child));
        id
    }

    /// Remove the child at `index` (positional). Kept for backward-compat.
    pub fn remove_child(&mut self, index: usize) -> Option<Box<dyn Component>> {
        if index < self.children.len() {
            Some(self.children.remove(index).1)
        } else {
            None
        }
    }

    /// Remove a child by its stable id.
    pub fn remove_child_by_id(&mut self, id: ComponentId) -> Option<Box<dyn Component>> {
        let pos = self.children.iter().position(|(cid, _)| *cid == id)?;
        Some(self.children.remove(pos).1)
    }

    /// Look up a child by id (immutable).
    pub fn child_by_id(&self, id: ComponentId) -> Option<&dyn Component> {
        self.children
            .iter()
            .find(|(cid, _)| *cid == id)
            .map(|(_, c)| c.as_ref())
    }

    /// Look up a child by id (mutable).
    pub fn child_by_id_mut(&mut self, id: ComponentId) -> Option<&mut dyn Component> {
        for (cid, child) in &mut self.children {
            if *cid == id {
                return Some(child.as_mut());
            }
        }
        None
    }

    /// Borrow all children. Ids are not exposed here to keep the pre-existing
    /// API stable; use [`Self::child_ids`] when needed.
    pub fn children(&self) -> Vec<&dyn Component> {
        self.children.iter().map(|(_, c)| c.as_ref()).collect()
    }

    /// Mutable view of the children.
    pub fn children_mut(&mut self) -> Vec<&mut dyn Component> {
        let mut out: Vec<&mut dyn Component> = Vec::with_capacity(self.children.len());
        for (_, child) in &mut self.children {
            out.push(child.as_mut());
        }
        out
    }

    /// Stable ids of all children, in insertion order.
    pub fn child_ids(&self) -> Vec<ComponentId> {
        self.children.iter().map(|(id, _)| *id).collect()
    }

    pub fn child_count(&self) -> usize {
        self.children.len()
    }

    pub fn clear(&mut self) {
        self.children.clear();
    }

    /// Dispatch an event to the focused child first, falling back to the
    /// reverse-order iteration used by [`Component::handle_input`] when the
    /// focused child returns `Ignored` (or no focus is set).
    pub fn dispatch_to_focused(
        &mut self,
        focus: Option<ComponentId>,
        event: &InputEvent,
    ) -> HandleResult {
        if let Some(id) = focus
            && let Some(child) = self.child_by_id_mut(id)
            && !child.is_hidden()
            && child.handle_input(event) == HandleResult::Handled
        {
            return HandleResult::Handled;
        }
        // Fallback: rev-order dispatch, but skip the already-tried focused id.
        for (cid, child) in self.children.iter_mut().rev() {
            if Some(*cid) == focus {
                continue;
            }
            if child.is_hidden() {
                continue;
            }
            if child.handle_input(event) == HandleResult::Handled {
                return HandleResult::Handled;
            }
        }
        HandleResult::Ignored
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
        for (_, child) in &self.children {
            if child.is_hidden() {
                continue;
            }
            lines.extend(child.render(width));
        }
        lines
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        // Try each child in reverse order (last child = topmost). Skip hidden.
        for (_, child) in self.children.iter_mut().rev() {
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
        for (_, child) in &mut self.children {
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

/// Result returned by an [`InputListener`].
///
/// `consume = true` short-circuits the rest of the dispatch pipeline (no
/// subsequent listener runs and component dispatch is skipped). `data =
/// Some(_)` rewrites the raw payload that downstream listeners and components
/// receive — used for legacy compatibility shims (e.g. translating one escape
/// sequence into another before any component sees it).
#[derive(Debug, Clone, Default)]
pub struct ListenerResult {
    pub consume: bool,
    pub data: Option<String>,
}

impl ListenerResult {
    /// Pass-through: don't consume, don't rewrite.
    pub fn pass() -> Self {
        Self::default()
    }

    /// Consume the event; downstream sees nothing.
    pub fn consume() -> Self {
        Self {
            consume: true,
            data: None,
        }
    }
}

/// Boxed input listener. Listeners are invoked BEFORE component dispatch and
/// can consume events or rewrite their payload (for `Raw` events). They run in
/// registration order.
pub type InputListener = Box<dyn FnMut(&InputEvent) -> ListenerResult + Send>;

/// Stable handle returned by [`Tui::add_input_listener`] for later removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenerId(u64);

/// Render tick interval. Mirrors the default in pi-tui (~4ms ≈ 240Hz cap).
const RENDER_TICK_MS: u64 = 4;

/// Main TUI engine: owns the terminal, the component tree, and the run loop.
pub struct Tui {
    terminal: Box<dyn Terminal>,
    root: Container,
    renderer: DiffRenderer,
    focus: Option<ComponentId>,
    listeners: Vec<(ListenerId, InputListener)>,
    next_listener_id: u64,
    running: Arc<AtomicBool>,
    render_requested: Arc<AtomicBool>,
    force_render: Arc<AtomicBool>,
    previous_width: u16,
    previous_height: u16,
    max_lines_rendered: usize,
    clear_on_shrink: bool,
    show_hardware_cursor: bool,
}

impl Tui {
    /// Create a new TUI with the given terminal backend.
    pub fn new(terminal: Box<dyn Terminal>) -> Self {
        let (cols, rows) = (terminal.columns(), terminal.rows());
        Self {
            terminal,
            root: Container::new(),
            renderer: DiffRenderer::new(),
            focus: None,
            listeners: Vec::new(),
            next_listener_id: 0,
            running: Arc::new(AtomicBool::new(false)),
            render_requested: Arc::new(AtomicBool::new(false)),
            force_render: Arc::new(AtomicBool::new(false)),
            previous_width: cols,
            previous_height: rows,
            max_lines_rendered: 0,
            clear_on_shrink: false,
            show_hardware_cursor: false,
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

    /// Set (or clear) the focused child.
    pub fn set_focus(&mut self, target: Option<ComponentId>) {
        self.focus = target;
    }

    /// Currently focused child id, if any.
    pub fn focus(&self) -> Option<ComponentId> {
        self.focus
    }

    /// Register a listener that runs before component dispatch. Returns a
    /// [`ListenerId`] that can be passed to [`Self::remove_input_listener`].
    pub fn add_input_listener(&mut self, listener: InputListener) -> ListenerId {
        let id = ListenerId(self.next_listener_id);
        self.next_listener_id += 1;
        self.listeners.push((id, listener));
        id
    }

    /// Unregister a listener previously added via [`Self::add_input_listener`].
    /// No-op if the id is unknown (already removed).
    pub fn remove_input_listener(&mut self, id: ListenerId) {
        self.listeners.retain(|(lid, _)| *lid != id);
    }

    /// Schedule a render on the next tick.
    pub fn request_render(&self) {
        self.render_requested.store(true, Ordering::Relaxed);
    }

    /// Schedule a render on the next tick, bypassing the diff cache (full
    /// re-render). Useful after operations that may have invalidated terminal
    /// state outside the renderer's knowledge (e.g. external `stty`).
    pub fn request_render_force(&self) {
        self.force_render.store(true, Ordering::Relaxed);
        self.render_requested.store(true, Ordering::Relaxed);
    }

    /// When true, shrinking the terminal triggers a full clear before
    /// re-rendering. Defaults to off.
    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        self.clear_on_shrink = enabled;
    }

    /// When true, the hardware cursor is left visible after each render.
    /// Defaults to off (cursor hidden).
    pub fn set_show_hardware_cursor(&mut self, enabled: bool) {
        self.show_hardware_cursor = enabled;
    }

    /// Stop the run loop. Safe to call from any thread.
    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    /// Get terminal dimensions `(columns, rows)` from the latest snapshot.
    pub fn size(&self) -> (u16, u16) {
        (self.terminal.columns(), self.terminal.rows())
    }

    /// Run the async event loop. Returns when [`Self::stop`] is called or
    /// stdin closes (EOF).
    pub async fn run(&mut self) -> TuiResult<()> {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        self.running.store(true, Ordering::Relaxed);
        let running = self.running.clone();
        tokio::spawn(async move {
            let _ = run_stdin_reader(event_tx, running).await;
        });
        self.run_with_events(event_rx).await
    }

    /// Test-friendly entry point: run with a pre-built event source. Production
    /// callers use [`Self::run`] which spawns its own stdin reader.
    pub async fn run_with_events(
        &mut self,
        mut events: mpsc::UnboundedReceiver<StdinBufferEvent>,
    ) -> TuiResult<()> {
        self.running.store(true, Ordering::Relaxed);

        // Resize channel — populated by M2.T4. Holding the sender keeps the
        // receiver open without ever firing.
        let (_resize_tx, mut resize_rx) = mpsc::unbounded_channel::<(u16, u16)>();

        let mut tick = time::interval(Duration::from_millis(RENDER_TICK_MS));
        tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        // Force the first frame.
        self.request_render_force();

        while self.running.load(Ordering::Relaxed) {
            tokio::select! {
                maybe_event = events.recv() => {
                    match maybe_event {
                        Some(event) => self.process_stdin_event(event),
                        None => {
                            // Stdin closed — exit cleanly.
                            break;
                        }
                    }
                }
                Some((cols, rows)) = resize_rx.recv() => {
                    self.process_resize(cols, rows);
                }
                _ = tick.tick() => {
                    self.maybe_render();
                }
            }
        }

        self.shutdown_terminal();
        Ok(())
    }

    /// Convert a [`StdinBufferEvent`] into an [`InputEvent`], run listeners,
    /// then dispatch.
    fn process_stdin_event(&mut self, event: StdinBufferEvent) {
        let data = match event {
            StdinBufferEvent::Data(s) => s,
            StdinBufferEvent::Overflow => return, // signal-only; nothing to do
        };

        let event = build_input_event(&data);
        self.dispatch_event(event);
        self.render_requested.store(true, Ordering::Relaxed);
    }

    /// Run listeners then dispatch to the focused component (with fallback).
    fn dispatch_event(&mut self, event: InputEvent) {
        // Listener phase — may consume or rewrite the payload.
        let mut current = event;
        for (_, listener) in &mut self.listeners {
            let result = listener(&current);
            if result.consume {
                return;
            }
            if let Some(replacement) = result.data {
                current = match current {
                    InputEvent::Raw(_) => InputEvent::Raw(replacement),
                    InputEvent::Paste(_) => InputEvent::Paste(replacement),
                    other => other, // data substitution only meaningful for raw payloads
                };
            }
        }

        // Component phase — focused first, fallback to rev-order.
        self.root.dispatch_to_focused(self.focus, &current);
    }

    fn process_resize(&mut self, cols: u16, rows: u16) {
        if cols != self.previous_width || rows != self.previous_height {
            self.force_render.store(true, Ordering::Relaxed);
            self.render_requested.store(true, Ordering::Relaxed);
        }
        // Forward to root so layout-sensitive components can react.
        self.root.handle_input(&InputEvent::Resize { cols, rows });
    }

    /// If a render is pending (or forced), draw a new frame.
    fn maybe_render(&mut self) {
        if !self.render_requested.swap(false, Ordering::Relaxed) {
            return;
        }
        let force = self.force_render.swap(false, Ordering::Relaxed);

        let (width, height) = (self.terminal.columns(), self.terminal.rows());
        let size_changed = width != self.previous_width || height != self.previous_height;
        let force = force || size_changed;

        if force {
            self.renderer.reset();
            if self.clear_on_shrink && (width < self.previous_width || height < self.previous_height) {
                self.terminal.clear_from_cursor();
            }
        }

        let lines = self.root.render(width);
        let commands = self.renderer.diff(&lines);
        if !commands.is_empty() {
            self.terminal.write(&commands);
        }

        self.previous_width = width;
        self.previous_height = height;
        if lines.len() > self.max_lines_rendered {
            self.max_lines_rendered = lines.len();
        }
    }

    /// Restore terminal state on exit.
    fn shutdown_terminal(&mut self) {
        if !self.show_hardware_cursor {
            self.terminal.show_cursor();
        }
        self.terminal.clear_from_cursor();
    }
}

/// Build an [`InputEvent`] from a stdin sequence. Sequences that start with
/// ESC or a single non-printable byte get parsed via `keys::parse_key`;
/// everything else is delivered as `Raw` so components handle plain text
/// (and bracketed paste payloads) themselves.
fn build_input_event(data: &str) -> InputEvent {
    let bytes = data.as_bytes();
    let parse_as_key = match bytes.first() {
        Some(0x1b) => true,                      // ESC-prefixed sequence
        Some(b) if bytes.len() == 1 && (*b < 0x20 || *b == 0x7f) => true, // ctrl/del
        _ => false,
    };

    if parse_as_key {
        InputEvent::Key(crate::keys::parse_key(data))
    } else {
        InputEvent::Raw(data.to_string())
    }
}

/// Stdin reader task: pumps bytes from `tokio::io::stdin()` through a
/// [`StdinBuffer`] and forwards each [`StdinBufferEvent`] over the channel.
async fn run_stdin_reader(
    sender: mpsc::UnboundedSender<StdinBufferEvent>,
    running: Arc<AtomicBool>,
) -> TuiResult<()> {
    let mut buffer = StdinBuffer::new();
    let mut stdin = tokio::io::stdin();
    let mut buf = [0u8; 4096];
    loop {
        if !running.load(Ordering::Relaxed) {
            break;
        }
        let n = stdin
            .read(&mut buf)
            .await
            .map_err(TuiError::Io)?;
        if n == 0 {
            break; // EOF
        }
        for event in buffer.push(&buf[..n]) {
            if sender.send(event).is_err() {
                return Ok(()); // receiver dropped — clean shutdown
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicU32;

    use crate::terminal::TestTerminal;

    // ---------- helpers ----------

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

    /// Component that records every event it receives and reports `Handled`
    /// (unless `ignore` is set, in which case it reports `Ignored`).
    struct RecordingComponent {
        events: Arc<Mutex<Vec<InputEvent>>>,
        ignore: bool,
    }

    impl RecordingComponent {
        fn new() -> (Self, Arc<Mutex<Vec<InputEvent>>>) {
            let events = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    events: events.clone(),
                    ignore: false,
                },
                events,
            )
        }

        fn ignoring() -> (Self, Arc<Mutex<Vec<InputEvent>>>) {
            let events = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    events: events.clone(),
                    ignore: true,
                },
                events,
            )
        }
    }

    impl Component for RecordingComponent {
        fn render(&self, _width: u16) -> Vec<String> {
            Vec::new()
        }
        fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
            self.events.lock().unwrap().push(event.clone());
            if self.ignore {
                HandleResult::Ignored
            } else {
                HandleResult::Handled
            }
        }
    }

    fn raw_event(s: &str) -> InputEvent {
        InputEvent::Raw(s.to_string())
    }

    fn make_tui() -> Tui {
        Tui::new(Box::new(TestTerminal::new(80, 24)))
    }

    // ---------- container behavior (existing + new) ----------

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

        let lines = container.render(80);
        assert_eq!(lines, vec!["visible"]);

        container.handle_input(&InputEvent::Raw("x".into()));
        assert_eq!(visible_count.get(), 1, "visible child must receive input");
        assert_eq!(hidden_count.get(), 0, "hidden child must NOT receive input");
    }

    // ---------- ComponentId ----------

    #[test]
    fn test_component_id_uniqueness() {
        let mut container = Container::new();
        let a = container.add_child_with_id(Box::new(TestComponent::new(vec!["a"])));
        let b = container.add_child_with_id(Box::new(TestComponent::new(vec!["b"])));
        let c = container.add_child_with_id(Box::new(TestComponent::new(vec!["c"])));
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
    }

    #[test]
    fn test_remove_child_by_id() {
        let mut container = Container::new();
        let a = container.add_child_with_id(Box::new(TestComponent::new(vec!["a"])));
        let b = container.add_child_with_id(Box::new(TestComponent::new(vec!["b"])));

        assert!(container.child_by_id(a).is_some());
        let removed = container.remove_child_by_id(a);
        assert!(removed.is_some());
        assert!(container.child_by_id(a).is_none());
        // `b` survives and ids never collide.
        assert!(container.child_by_id(b).is_some());
        assert_eq!(container.child_count(), 1);
    }

    // ---------- focus dispatch ----------

    #[test]
    fn test_focus_dispatch_routes_to_id() {
        let mut container = Container::new();
        let (a_comp, a_events) = RecordingComponent::new();
        let (b_comp, b_events) = RecordingComponent::new();
        container.add_child_with_id(Box::new(a_comp));
        let b_id = container.add_child_with_id(Box::new(b_comp));

        let result = container.dispatch_to_focused(Some(b_id), &raw_event("hi"));
        assert_eq!(result, HandleResult::Handled);
        assert_eq!(a_events.lock().unwrap().len(), 0);
        assert_eq!(b_events.lock().unwrap().len(), 1);
    }

    #[test]
    fn test_focus_dispatch_falls_through_on_ignored() {
        let mut container = Container::new();
        let (focused_comp, focused_events) = RecordingComponent::ignoring();
        let (sibling_comp, sibling_events) = RecordingComponent::new();
        let focused_id = container.add_child_with_id(Box::new(focused_comp));
        container.add_child_with_id(Box::new(sibling_comp));

        let result = container.dispatch_to_focused(Some(focused_id), &raw_event("x"));
        assert_eq!(result, HandleResult::Handled);
        // Focused saw it first but ignored.
        assert_eq!(focused_events.lock().unwrap().len(), 1);
        // Fallback found the sibling.
        assert_eq!(sibling_events.lock().unwrap().len(), 1);
    }

    // ---------- listeners ----------

    #[test]
    fn test_listener_can_consume_event() {
        let mut tui = make_tui();
        let (comp, events) = RecordingComponent::new();
        tui.root_mut().add_child_with_id(Box::new(comp));

        tui.add_input_listener(Box::new(|_e| ListenerResult::consume()));
        tui.dispatch_event(raw_event("blocked"));

        assert!(events.lock().unwrap().is_empty(), "consumed event should not reach component");
    }

    #[test]
    fn test_listener_data_substitution() {
        let mut tui = make_tui();
        let (comp, events) = RecordingComponent::new();
        tui.root_mut().add_child_with_id(Box::new(comp));

        tui.add_input_listener(Box::new(|_e| ListenerResult {
            consume: false,
            data: Some("REPLACED".into()),
        }));
        tui.dispatch_event(raw_event("x"));

        let received = events.lock().unwrap();
        assert_eq!(received.len(), 1);
        match &received[0] {
            InputEvent::Raw(s) => assert_eq!(s, "REPLACED"),
            other => panic!("expected Raw, got {other:?}"),
        }
    }

    #[test]
    fn test_remove_input_listener() {
        let mut tui = make_tui();
        let (comp, events) = RecordingComponent::new();
        tui.root_mut().add_child_with_id(Box::new(comp));

        let id = tui.add_input_listener(Box::new(|_e| ListenerResult::consume()));
        tui.remove_input_listener(id);
        tui.dispatch_event(raw_event("x"));

        assert_eq!(events.lock().unwrap().len(), 1, "after removal, event reaches component");
    }

    // ---------- render flag ----------

    #[test]
    fn test_request_render_sets_flag() {
        let tui = make_tui();
        assert!(!tui.render_requested.load(Ordering::Relaxed));
        tui.request_render();
        assert!(tui.render_requested.load(Ordering::Relaxed));
    }

    #[test]
    fn test_request_render_force_sets_both_flags() {
        let tui = make_tui();
        tui.request_render_force();
        assert!(tui.render_requested.load(Ordering::Relaxed));
        assert!(tui.force_render.load(Ordering::Relaxed));
    }

    // ---------- run loop integration ----------

    #[tokio::test]
    async fn test_stop_stops_run_loop() {
        let mut tui = make_tui();
        let running = tui.running.clone();
        let (_tx, rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_millis(500), tui.run_with_events(rx)).await;
        });

        // Give the loop a moment to start, then stop it.
        tokio::time::sleep(Duration::from_millis(20)).await;
        running.store(false, Ordering::Relaxed);

        // Should exit promptly (well under 500ms).
        let _ = tokio::time::timeout(Duration::from_millis(200), handle)
            .await
            .expect("run loop did not stop within 200ms");
    }

    #[tokio::test]
    async fn test_run_dispatches_stdin_event_to_root() {
        let mut tui = make_tui();
        let (comp, events) = RecordingComponent::new();
        tui.root_mut().add_child_with_id(Box::new(comp));

        let running = tui.running.clone();
        let (tx, rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(async move { tui.run_with_events(rx).await });

        // Wait for the loop to come up.
        tokio::time::sleep(Duration::from_millis(20)).await;

        tx.send(StdinBufferEvent::Data("hello".into())).unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;

        running.store(false, Ordering::Relaxed);
        drop(tx);
        let _ = tokio::time::timeout(Duration::from_millis(200), handle).await;

        let received = events.lock().unwrap();
        assert_eq!(received.len(), 1);
        match &received[0] {
            InputEvent::Raw(s) => assert_eq!(s, "hello"),
            other => panic!("expected Raw('hello'), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_run_exits_on_stdin_close() {
        let mut tui = make_tui();
        let (tx, rx) = mpsc::unbounded_channel();
        // Drop sender immediately — channel closes.
        drop(tx);

        let result = tokio::time::timeout(Duration::from_millis(200), tui.run_with_events(rx))
            .await
            .expect("run did not exit on stdin close");
        assert!(result.is_ok());
    }

    // ---------- input event classification ----------

    #[test]
    fn test_build_input_event_classifies_escape_as_key() {
        match build_input_event("\x1b[A") {
            InputEvent::Key(_) => {}
            other => panic!("expected Key for escape sequence, got {other:?}"),
        }
    }

    #[test]
    fn test_build_input_event_classifies_printable_as_raw() {
        match build_input_event("hello") {
            InputEvent::Raw(s) => assert_eq!(s, "hello"),
            other => panic!("expected Raw for printable, got {other:?}"),
        }
    }

    #[test]
    fn test_build_input_event_classifies_ctrl_as_key() {
        match build_input_event("\x03") {
            InputEvent::Key(_) => {}
            other => panic!("expected Key for ctrl byte, got {other:?}"),
        }
    }
}
