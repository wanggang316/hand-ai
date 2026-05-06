//! Integration tests for `Tui` composition + `DiffRenderer`.

mod common;

use hand_tui::{
    BoxComponent, Component, Container, DiffRenderer, HandleResult, InputEvent, SpacerComponent,
    TestTerminal, TextComponent, Tui,
};

#[test]
fn empty_diff_renderer_first_call_emits_full_render() {
    let mut renderer = DiffRenderer::new();
    let out = renderer.diff(&["hello".to_string(), "world".to_string()]);
    assert!(out.contains("hello"));
    assert!(out.contains("world"));
    assert_eq!(renderer.prev_line_count(), 2);
}

#[test]
fn diff_returns_empty_string_when_unchanged() {
    let mut renderer = DiffRenderer::new();
    let _ = renderer.diff(&["a".to_string()]);
    let second = renderer.diff(&["a".to_string()]);
    assert!(second.is_empty(), "got {:?}", second);
}

#[test]
fn diff_emits_clear_when_a_line_changes() {
    let mut renderer = DiffRenderer::new();
    let _ = renderer.diff(&["a".to_string(), "b".to_string()]);
    let out = renderer.diff(&["a".to_string(), "B".to_string()]);
    assert!(out.contains('B'));
    assert!(out.contains("\x1b[2K"));
}

#[test]
fn renderer_reset_forces_full_render_again() {
    let mut renderer = DiffRenderer::new();
    let _ = renderer.diff(&["a".to_string()]);
    renderer.reset();
    let out = renderer.diff(&["a".to_string()]);
    // After reset, the next diff is treated as the first frame again.
    assert!(out.contains('a'));
}

#[test]
fn container_tracks_children_by_id() {
    let mut c = Container::new();
    let id_a = c.add_child_with_id(Box::new(TextComponent::new("a")));
    let id_b = c.add_child_with_id(Box::new(TextComponent::new("b")));
    assert_eq!(c.child_count(), 2);
    assert_ne!(id_a, id_b);
    assert_eq!(c.child_ids(), vec![id_a, id_b]);
    let removed = c.remove_child_by_id(id_a);
    assert!(removed.is_some());
    assert_eq!(c.child_count(), 1);
}

#[test]
fn container_lookup_returns_none_after_removal() {
    let mut c = Container::new();
    let real = c.add_child_with_id(Box::new(TextComponent::new("x")));
    let _ = c.remove_child_by_id(real);
    assert!(c.child_by_id(real).is_none());
}

#[test]
fn tui_with_test_terminal_exposes_size_and_root() {
    let term = TestTerminal::new(60, 20);
    let mut tui = Tui::new(Box::new(term));
    let id = tui.root_mut().add_child_with_id(Box::new(TextComponent::new("hi")));
    assert_eq!(tui.size(), (60, 20));
    assert_eq!(tui.root().child_count(), 1);
    tui.set_focus(Some(id));
    assert_eq!(tui.focus(), Some(id));
}

#[test]
fn box_and_spacer_are_renderable() {
    let mut bx = BoxComponent::new().with_padding(1, 0);
    bx.add_child(Box::new(TextComponent::new("hi")));
    let lines = bx.render(20);
    assert!(!lines.is_empty());
    let sp = SpacerComponent::new(2);
    let lines = sp.render(20);
    assert_eq!(lines.len(), 2);
}

#[test]
fn dispatch_to_focused_uses_focus_first() {
    struct Counter {
        consumed: bool,
    }
    impl Component for Counter {
        fn render(&self, _w: u16) -> Vec<String> {
            vec![]
        }
        fn handle_input(&mut self, _event: &InputEvent) -> HandleResult {
            self.consumed = true;
            HandleResult::Handled
        }
    }
    let mut c = Container::new();
    let id = c.add_child_with_id(Box::new(Counter { consumed: false }));
    let result = c.dispatch_to_focused(Some(id), &InputEvent::Raw("x".into()));
    assert_eq!(result, HandleResult::Handled);
}
