//! Terminal abstraction layer.
//!
//! Provides a trait for terminal I/O operations and a real implementation
//! using crossterm.

use crossterm::terminal;

/// Terminal capabilities detection.
#[derive(Debug, Clone)]
pub struct TerminalCapabilities {
    pub supports_color: bool,
    pub supports_unicode: bool,
    pub supports_images: bool,
    pub supports_mouse: bool,
    pub supports_kitty_protocol: bool,
}

impl Default for TerminalCapabilities {
    fn default() -> Self {
        Self {
            supports_color: true,
            supports_unicode: true,
            supports_images: false,
            supports_mouse: true,
            supports_kitty_protocol: false,
        }
    }
}

/// Abstract terminal interface.
pub trait Terminal: Send {
    /// Write data to the terminal.
    fn write(&mut self, data: &str);

    /// Get terminal width in columns.
    fn columns(&self) -> u16;

    /// Get terminal height in rows.
    fn rows(&self) -> u16;

    /// Hide the cursor.
    fn hide_cursor(&mut self);

    /// Show the cursor.
    fn show_cursor(&mut self);

    /// Clear the current line.
    fn clear_line(&mut self);

    /// Clear from cursor to end of screen.
    fn clear_from_cursor(&mut self);

    /// Clear the entire screen.
    fn clear_screen(&mut self);

    /// Move cursor by the given number of lines (positive = down).
    fn move_by(&mut self, lines: i32);

    /// Set the terminal title.
    fn set_title(&mut self, title: &str);

    /// Get terminal capabilities.
    fn capabilities(&self) -> &TerminalCapabilities;
}

/// Real terminal implementation using crossterm.
pub struct ProcessTerminal {
    capabilities: TerminalCapabilities,
    columns: u16,
    rows: u16,
}

impl ProcessTerminal {
    pub fn new() -> std::io::Result<Self> {
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        Ok(Self {
            capabilities: TerminalCapabilities::default(),
            columns: cols,
            rows,
        })
    }

    /// Refresh terminal size (call after resize).
    pub fn refresh_size(&mut self) {
        if let Ok((cols, rows)) = terminal::size() {
            self.columns = cols;
            self.rows = rows;
        }
    }
}

impl Default for ProcessTerminal {
    fn default() -> Self {
        Self::new().unwrap_or(Self {
            capabilities: TerminalCapabilities::default(),
            columns: 80,
            rows: 24,
        })
    }
}

impl Terminal for ProcessTerminal {
    fn write(&mut self, data: &str) {
        use std::io::Write;
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(data.as_bytes());
        let _ = stdout.flush();
    }

    fn columns(&self) -> u16 {
        self.columns
    }

    fn rows(&self) -> u16 {
        self.rows
    }

    fn hide_cursor(&mut self) {
        self.write("\x1b[?25l");
    }

    fn show_cursor(&mut self) {
        self.write("\x1b[?25h");
    }

    fn clear_line(&mut self) {
        self.write("\x1b[2K\r");
    }

    fn clear_from_cursor(&mut self) {
        self.write("\x1b[J");
    }

    fn clear_screen(&mut self) {
        self.write("\x1b[2J\x1b[H");
    }

    fn move_by(&mut self, lines: i32) {
        if lines > 0 {
            self.write(&format!("\x1b[{}B", lines));
        } else if lines < 0 {
            self.write(&format!("\x1b[{}A", -lines));
        }
    }

    fn set_title(&mut self, title: &str) {
        self.write(&format!("\x1b]0;{}\x07", title));
    }

    fn capabilities(&self) -> &TerminalCapabilities {
        &self.capabilities
    }
}

/// In-memory terminal for testing.
pub struct TestTerminal {
    pub output: Vec<String>,
    pub columns: u16,
    pub rows: u16,
    capabilities: TerminalCapabilities,
}

impl TestTerminal {
    pub fn new(columns: u16, rows: u16) -> Self {
        Self {
            output: Vec::new(),
            columns,
            rows,
            capabilities: TerminalCapabilities::default(),
        }
    }

    pub fn last_output(&self) -> Option<&str> {
        self.output.last().map(|s| s.as_str())
    }
}

impl Terminal for TestTerminal {
    fn write(&mut self, data: &str) {
        self.output.push(data.to_string());
    }

    fn columns(&self) -> u16 {
        self.columns
    }

    fn rows(&self) -> u16 {
        self.rows
    }

    fn hide_cursor(&mut self) {
        self.write("\x1b[?25l");
    }

    fn show_cursor(&mut self) {
        self.write("\x1b[?25h");
    }

    fn clear_line(&mut self) {
        self.write("\x1b[2K\r");
    }

    fn clear_from_cursor(&mut self) {
        self.write("\x1b[J");
    }

    fn clear_screen(&mut self) {
        self.write("\x1b[2J\x1b[H");
    }

    fn move_by(&mut self, lines: i32) {
        if lines > 0 {
            self.write(&format!("\x1b[{}B", lines));
        } else if lines < 0 {
            self.write(&format!("\x1b[{}A", -lines));
        }
    }

    fn set_title(&mut self, title: &str) {
        self.write(&format!("\x1b]0;{}\x07", title));
    }

    fn capabilities(&self) -> &TerminalCapabilities {
        &self.capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_test_terminal_basic() {
        let mut term = TestTerminal::new(80, 24);
        assert_eq!(term.columns(), 80);
        assert_eq!(term.rows(), 24);

        term.write("hello");
        assert_eq!(term.last_output(), Some("hello"));
    }

    #[test]
    fn test_test_terminal_cursor() {
        let mut term = TestTerminal::new(80, 24);
        term.hide_cursor();
        assert_eq!(term.output.last().unwrap(), "\x1b[?25l");
        term.show_cursor();
        assert_eq!(term.output.last().unwrap(), "\x1b[?25h");
    }

    #[test]
    fn test_capabilities_default() {
        let caps = TerminalCapabilities::default();
        assert!(caps.supports_color);
        assert!(caps.supports_unicode);
        assert!(!caps.supports_images);
    }

    #[test]
    fn test_move_by() {
        let mut term = TestTerminal::new(80, 24);
        term.move_by(3);
        assert_eq!(term.output.last().unwrap(), "\x1b[3B");
        term.move_by(-2);
        assert_eq!(term.output.last().unwrap(), "\x1b[2A");
        let len = term.output.len();
        term.move_by(0);
        assert_eq!(term.output.len(), len); // No output for 0 movement
    }
}
