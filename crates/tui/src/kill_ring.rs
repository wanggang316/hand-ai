//! Kill ring — clipboard-like circular buffer for cut/paste operations.

/// A circular buffer of killed (cut) text, similar to Emacs kill ring.
#[derive(Debug, Clone)]
pub struct KillRing {
    entries: Vec<String>,
    max_size: usize,
    yank_index: Option<usize>,
}

impl Default for KillRing {
    fn default() -> Self {
        Self::new(32)
    }
}

impl KillRing {
    /// Create a new kill ring with the given maximum size.
    pub fn new(max_size: usize) -> Self {
        Self {
            entries: Vec::new(),
            max_size: max_size.max(1),
            yank_index: None,
        }
    }

    /// Push text onto the kill ring.
    pub fn push(&mut self, text: String) {
        if text.is_empty() {
            return;
        }
        self.entries.push(text);
        if self.entries.len() > self.max_size {
            self.entries.remove(0);
        }
        self.yank_index = None;
    }

    /// Append text to the last kill (for consecutive kill operations).
    pub fn append_to_last(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.entries.last_mut() {
            last.push_str(text);
        } else {
            self.push(text.to_string());
        }
        self.yank_index = None;
    }

    /// Yank (paste) the most recent kill.
    pub fn yank(&mut self) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }
        let idx = self.entries.len() - 1;
        self.yank_index = Some(idx);
        Some(&self.entries[idx])
    }

    /// Cycle to the next older entry in the ring (yank-pop).
    ///
    /// Must be called after `yank()`. Returns the next entry, cycling around.
    pub fn yank_pop(&mut self) -> Option<&str> {
        let idx = self.yank_index?;
        if self.entries.is_empty() {
            return None;
        }
        let new_idx = if idx == 0 {
            self.entries.len() - 1
        } else {
            idx - 1
        };
        self.yank_index = Some(new_idx);
        Some(&self.entries[new_idx])
    }

    /// Reset the yank position (call after non-yank operations).
    pub fn reset(&mut self) {
        self.yank_index = None;
    }

    /// Check if the ring is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get the number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ring_is_empty() {
        let ring = KillRing::new(10);
        assert!(ring.is_empty());
        assert_eq!(ring.len(), 0);
    }

    #[test]
    fn push_and_yank() {
        let mut ring = KillRing::new(10);
        ring.push("hello".to_string());
        assert_eq!(ring.yank(), Some("hello"));
    }

    #[test]
    fn yank_pop_cycles() {
        let mut ring = KillRing::new(10);
        ring.push("first".to_string());
        ring.push("second".to_string());
        ring.push("third".to_string());

        assert_eq!(ring.yank(), Some("third"));
        assert_eq!(ring.yank_pop(), Some("second"));
        assert_eq!(ring.yank_pop(), Some("first"));
        // Cycles back to last
        assert_eq!(ring.yank_pop(), Some("third"));
    }

    #[test]
    fn max_size_enforced() {
        let mut ring = KillRing::new(3);
        ring.push("a".to_string());
        ring.push("b".to_string());
        ring.push("c".to_string());
        ring.push("d".to_string());
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.yank(), Some("d"));
    }

    #[test]
    fn append_to_last() {
        let mut ring = KillRing::new(10);
        ring.push("hello".to_string());
        ring.append_to_last(" world");
        assert_eq!(ring.yank(), Some("hello world"));
        assert_eq!(ring.len(), 1);
    }

    #[test]
    fn append_to_empty_creates_entry() {
        let mut ring = KillRing::new(10);
        ring.append_to_last("text");
        assert_eq!(ring.yank(), Some("text"));
    }

    #[test]
    fn empty_push_ignored() {
        let mut ring = KillRing::new(10);
        ring.push("".to_string());
        assert!(ring.is_empty());
    }

    #[test]
    fn yank_on_empty_returns_none() {
        let mut ring = KillRing::new(10);
        assert!(ring.yank().is_none());
    }

    #[test]
    fn yank_pop_without_yank_returns_none() {
        let mut ring = KillRing::new(10);
        ring.push("hello".to_string());
        assert!(ring.yank_pop().is_none());
    }

    #[test]
    fn reset_clears_yank_position() {
        let mut ring = KillRing::new(10);
        ring.push("hello".to_string());
        ring.yank();
        ring.reset();
        assert!(ring.yank_pop().is_none());
    }

    #[test]
    fn default_creates_ring_of_32() {
        let ring = KillRing::default();
        assert_eq!(ring.max_size, 32);
    }
}
