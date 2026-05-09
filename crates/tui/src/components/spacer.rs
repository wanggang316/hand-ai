//! Spacer component — empty vertical space.
//
// audit: M3.T5 — parity confirmed against pi-tui/spacer.ts on 2026-05-07.

use crate::tui::Component;

/// Empty space of a given number of lines.
pub struct SpacerComponent {
    lines: u16,
}

impl SpacerComponent {
    pub fn new(lines: u16) -> Self {
        Self { lines }
    }

    /// Update the number of blank lines emitted by this spacer.
    pub fn set_lines(&mut self, lines: u16) {
        self.lines = lines;
    }

    /// Current number of lines.
    pub fn lines(&self) -> u16 {
        self.lines
    }
}

impl Component for SpacerComponent {
    fn render(&self, _width: u16) -> Vec<String> {
        (0..self.lines).map(|_| String::new()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spacer() {
        let spacer = SpacerComponent::new(3);
        let lines = spacer.render(80);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| l.is_empty()));
    }

    #[test]
    fn test_spacer_zero() {
        let spacer = SpacerComponent::new(0);
        let lines = spacer.render(80);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_spacer_set_lines() {
        let mut spacer = SpacerComponent::new(1);
        assert_eq!(spacer.lines(), 1);
        spacer.set_lines(5);
        assert_eq!(spacer.lines(), 5);
        assert_eq!(spacer.render(80).len(), 5);
    }
}
