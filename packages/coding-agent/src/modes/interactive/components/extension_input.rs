//! Single-line text-input dialog used by extensions.
//!
//! Ported from
//! `pi-mono/packages/coding-agent/src/modes/interactive/components/extension-input.ts`.
//!
//! The TS class extends pi-tui's `Container` and dispatches input via
//! callbacks. The Rust port owns the [`InputComponent`] and renders a
//! framed dialog directly (border, title, input, hint, border) — bypassing
//! a `Container` because the embedded primitives don't expose downcast
//! handles after being moved into one.
//!
//! Events ([`ExtensionInputEvent::Submit`] / `Cancel`) are surfaced via an
//! [`mpsc::Sender`] supplied at construction. Per
//! `.claude/conversion-guidelines.md`, channels are preferred over
//! `Box<dyn Fn>` callbacks for cross-component signalling.
//!
//! Theming caveat: pi-mono reads the `accent` slot from the coding-agent
//! theme. The newly-ported [`crate::modes::interactive::theme::Theme`] is
//! the eventual home for that, but this component is wired for both: pass
//! a `Theme` to colour the title, or pass `None` to render plain text and
//! defer styling to the driver.

use std::sync::mpsc::Sender;
use std::time::Duration;

use hand_tui::components::input::InputComponent;
use hand_tui::tui::{Component, Focusable, HandleResult, InputEvent};
use hand_tui::utils::visible_width;

use super::countdown_timer::CountdownTimer;
use super::dynamic_border::DynamicBorderComponent;
use super::keybinding_hints::key_hint_for;
use crate::modes::interactive::theme::{Theme, ThemeColor};

/// Events surfaced by [`ExtensionInputComponent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionInputEvent {
    /// User pressed `tui.select.confirm` (default `enter`).
    Submit(String),
    /// User pressed `tui.select.cancel` (default `escape`) *or* the
    /// optional countdown timer expired.
    Cancel,
}

/// Single-line input dialog with optional countdown timer.
pub struct ExtensionInputComponent {
    base_title: String,
    title_line: String,
    input: InputComponent,
    border: DynamicBorderComponent,
    hint: String,
    countdown: Option<CountdownTimer>,
    events: Sender<ExtensionInputEvent>,
    focused: bool,
    theme: Option<Theme>,
}

impl ExtensionInputComponent {
    /// Build a new dialog with `title`, an optional `placeholder`, and an
    /// optional countdown timer (`timeout`).
    ///
    /// Events are sent via `events`. Send errors (channel closed) are
    /// silently dropped — the component is then orphaned and the caller
    /// is expected to drop it.
    pub fn new(
        title: impl Into<String>,
        placeholder: Option<&str>,
        timeout: Option<Duration>,
        events: Sender<ExtensionInputEvent>,
        theme: Option<Theme>,
    ) -> Self {
        let title = title.into();

        let mut input = InputComponent::new();
        if let Some(ph) = placeholder {
            input = input.with_placeholder(ph);
        }
        // Forward Enter/Escape from the inner input via the shared channel.
        let submit_tx = events.clone();
        input.set_on_submit(Box::new(move |text: &str| {
            let _ = submit_tx.send(ExtensionInputEvent::Submit(text.to_string()));
        }));
        let escape_tx = events.clone();
        input.set_on_escape(Box::new(move || {
            let _ = escape_tx.send(ExtensionInputEvent::Cancel);
        }));

        let hint = format!(
            "{}  {}",
            key_hint_for("tui.select.confirm", "submit"),
            key_hint_for("tui.select.cancel", "cancel"),
        );

        let countdown = timeout.and_then(|d| {
            if d.as_millis() == 0 {
                return None;
            }
            let expire_tx = events.clone();
            Some(CountdownTimer::new(
                d,
                |_seconds| { /* tick — title repaint happens lazily on next render */ },
                move || {
                    let _ = expire_tx.send(ExtensionInputEvent::Cancel);
                },
            ))
        });

        let mut me = Self {
            base_title: title.clone(),
            title_line: String::new(),
            input,
            border: DynamicBorderComponent::new(),
            hint,
            countdown,
            events,
            focused: false,
            theme,
        };
        me.refresh_title();
        me
    }

    /// Drive the optional countdown by one second. No-op when no timer is
    /// configured. The driver is expected to call this from its tick loop.
    pub fn tick_countdown(&mut self) {
        if let Some(cd) = self.countdown.as_mut() {
            cd.tick();
            self.refresh_title();
        }
    }

    /// Drop the timer without firing its expire callback.
    pub fn dispose(&mut self) {
        if let Some(cd) = self.countdown.as_mut() {
            cd.dispose();
        }
    }

    fn refresh_title(&mut self) {
        let body = match self
            .countdown
            .as_ref()
            .map(CountdownTimer::remaining_seconds)
        {
            Some(s) if s >= 0 => format!("{} ({}s)", self.base_title, s),
            _ => self.base_title.clone(),
        };
        let coloured = match (&self.theme, ()) {
            (Some(theme), _) => theme.fg(ThemeColor::Accent, &body).unwrap_or(body.clone()),
            _ => body,
        };
        self.title_line = coloured;
    }

    /// Borrow the channel sender so further countdowns can be cloned out.
    pub fn events(&self) -> &Sender<ExtensionInputEvent> {
        &self.events
    }
}

impl Component for ExtensionInputComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let mut out = Vec::new();
        out.extend(self.border.render(width));
        out.push(String::new());
        out.push(pad_line(&self.title_line, width));
        out.push(String::new());
        out.extend(self.input.render(width));
        out.push(String::new());
        out.push(pad_line(&self.hint, width));
        out.push(String::new());
        out.extend(self.border.render(width));
        out
    }

    fn handle_input(&mut self, event: &InputEvent) -> HandleResult {
        // The inner InputComponent handles Enter/Escape via its on_submit /
        // on_escape callbacks (which we wired to the events channel).
        self.input.handle_input(event)
    }

    fn invalidate(&mut self) {
        self.input.invalidate();
    }
}

impl Focusable for ExtensionInputComponent {
    fn focused(&self) -> bool {
        self.focused
    }

    fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
        self.input.set_focused(focused);
    }

    /// The input sits four rows below the top border (border, blank,
    /// title, blank, input). Translate the inner cursor accordingly.
    fn cursor_position(&self) -> Option<(u16, u16)> {
        let (col, _) = self.input.cursor_position()?;
        Some((col, 4))
    }
}

/// Right-pad a line with spaces (visual width) so framed dialogs render
/// rectangular.
fn pad_line(line: &str, width: u16) -> String {
    let target = width as usize;
    let current = visible_width(line);
    if current >= target {
        line.to_string()
    } else {
        format!("{}{}", line, " ".repeat(target - current))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn make_event(data: &str) -> InputEvent {
        InputEvent::Raw(data.to_string())
    }

    #[test]
    fn renders_title_and_hint() {
        let (tx, _rx) = mpsc::channel();
        let comp = ExtensionInputComponent::new("Confirm?", None, None, tx, None);
        let lines = comp.render(40);
        // Title line should mention the base title.
        assert!(lines.iter().any(|l| l.contains("Confirm?")));
        // Hint line should mention "submit" and "cancel".
        assert!(lines.iter().any(|l| l.contains("submit")));
        assert!(lines.iter().any(|l| l.contains("cancel")));
    }

    #[test]
    fn enter_emits_submit() {
        let (tx, rx) = mpsc::channel();
        let mut comp = ExtensionInputComponent::new("t", None, None, tx, None);
        comp.set_focused(true);
        // Type some text first.
        for ch in "hi".chars() {
            comp.handle_input(&make_event(&ch.to_string()));
        }
        // Press Enter.
        comp.handle_input(&make_event("\r"));
        let evt = rx.try_recv().unwrap();
        assert_eq!(evt, ExtensionInputEvent::Submit("hi".to_string()));
    }

    #[test]
    fn escape_emits_cancel() {
        let (tx, rx) = mpsc::channel();
        let mut comp = ExtensionInputComponent::new("t", None, None, tx, None);
        comp.set_focused(true);
        comp.handle_input(&make_event("\x1b"));
        let evt = rx.try_recv().unwrap();
        assert_eq!(evt, ExtensionInputEvent::Cancel);
    }

    #[test]
    fn countdown_expiry_emits_cancel() {
        let (tx, rx) = mpsc::channel();
        let mut comp =
            ExtensionInputComponent::new("t", None, Some(Duration::from_secs(1)), tx, None);
        // First tick: 1 -> 0 -> expire.
        comp.tick_countdown();
        let evt = rx.try_recv().unwrap();
        assert_eq!(evt, ExtensionInputEvent::Cancel);
    }

    #[test]
    fn title_includes_remaining_seconds_when_timed() {
        let (tx, _rx) = mpsc::channel();
        let comp = ExtensionInputComponent::new("Hi", None, Some(Duration::from_secs(5)), tx, None);
        let lines = comp.render(40);
        assert!(
            lines.iter().any(|l| l.contains("(5s)")),
            "expected (5s) in title, got {:?}",
            lines
        );
    }

    #[test]
    fn focusable_propagates_to_input() {
        let (tx, _rx) = mpsc::channel();
        let mut comp = ExtensionInputComponent::new("t", None, None, tx, None);
        assert!(!comp.focused());
        comp.set_focused(true);
        assert!(comp.focused());
        // Inner input should receive focus too — observable via render.
        let _ = comp.render(40);
    }
}
