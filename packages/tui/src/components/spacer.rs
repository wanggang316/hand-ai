//! Spacer component — empty vertical space.

use crate::tui::Component;

/// Empty space of a given number of lines.
pub struct SpacerComponent {
    lines: u16,
}

impl SpacerComponent {
    pub fn new(lines: u16) -> Self {
        Self { lines }
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
}
