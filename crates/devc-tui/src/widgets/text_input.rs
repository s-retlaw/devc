//! Reusable text input state management
//!
//! Provides cursor-based text editing functionality that can be shared
//! between different input fields (settings, provider details, etc.)

/// State for a text input field with cursor support
#[derive(Debug, Clone, Default)]
pub struct TextInputState {
    /// The text being edited
    buffer: String,
    /// Current cursor position (byte offset)
    cursor: usize,
}

impl TextInputState {
    /// Create a new empty text input state
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            cursor: 0,
        }
    }

    /// Create a text input state with initial value
    ///
    /// Cursor is placed at the end of the initial value.
    pub fn with_value(value: &str) -> Self {
        let len = value.len();
        Self {
            buffer: value.to_string(),
            cursor: len,
        }
    }

    /// Reset the input to a new value
    ///
    /// Cursor is placed at the end.
    pub fn set_value(&mut self, value: &str) {
        self.buffer = value.to_string();
        self.cursor = self.buffer.len();
    }

    /// Get the current value
    pub fn value(&self) -> &str {
        &self.buffer
    }

    /// Take the value, consuming this state
    pub fn take(self) -> String {
        self.buffer
    }

    /// Get the current cursor position
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Insert a character at the current cursor position
    pub fn insert(&mut self, c: char) {
        self.buffer.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Delete the character at the cursor (like Delete key)
    pub fn delete(&mut self) {
        if self.cursor < self.buffer.len() {
            self.buffer.remove(self.cursor);
        }
    }

    /// Delete the character before the cursor (like Backspace)
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            // Find the previous character boundary
            let prev_cursor = self.prev_char_boundary();
            self.buffer.remove(prev_cursor);
            self.cursor = prev_cursor;
        }
    }

    /// Move cursor one character to the left
    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.prev_char_boundary();
        }
    }

    /// Move cursor one character to the right
    pub fn move_right(&mut self) {
        if self.cursor < self.buffer.len() {
            self.cursor = self.next_char_boundary();
        }
    }

    /// Move cursor to the beginning
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// Move cursor to the end
    pub fn end(&mut self) {
        self.cursor = self.buffer.len();
    }

    /// Clear the buffer and reset cursor
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
    }

    /// Check if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    /// Get the length of the buffer
    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    /// Get the text before the cursor
    pub fn before_cursor(&self) -> &str {
        &self.buffer[..self.cursor]
    }

    /// Get the text after the cursor
    pub fn after_cursor(&self) -> &str {
        &self.buffer[self.cursor..]
    }

    /// Find the previous character boundary
    fn prev_char_boundary(&self) -> usize {
        let mut idx = self.cursor.saturating_sub(1);
        while idx > 0 && !self.buffer.is_char_boundary(idx) {
            idx -= 1;
        }
        idx
    }

    /// Find the next character boundary
    fn next_char_boundary(&self) -> usize {
        let mut idx = self.cursor + 1;
        while idx < self.buffer.len() && !self.buffer.is_char_boundary(idx) {
            idx += 1;
        }
        idx.min(self.buffer.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_empty() {
        let input = TextInputState::new();
        assert!(input.is_empty());
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn test_with_value() {
        let input = TextInputState::with_value("hello");
        assert_eq!(input.value(), "hello");
        assert_eq!(input.cursor(), 5);
    }

    #[test]
    fn test_insert_at_end() {
        let mut input = TextInputState::with_value("hello");
        input.insert('!');
        assert_eq!(input.value(), "hello!");
        assert_eq!(input.cursor(), 6);
    }

    #[test]
    fn test_insert_in_middle() {
        let mut input = TextInputState::with_value("hllo");
        input.cursor = 1;
        input.insert('e');
        assert_eq!(input.value(), "hello");
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn test_insert_at_start() {
        let mut input = TextInputState::with_value("ello");
        input.cursor = 0;
        input.insert('h');
        assert_eq!(input.value(), "hello");
        assert_eq!(input.cursor(), 1);
    }

    #[test]
    fn test_backspace() {
        let mut input = TextInputState::with_value("hello");
        input.backspace();
        assert_eq!(input.value(), "hell");
        assert_eq!(input.cursor(), 4);
    }

    #[test]
    fn test_backspace_at_start() {
        let mut input = TextInputState::with_value("hello");
        input.cursor = 0;
        input.backspace();
        assert_eq!(input.value(), "hello");
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn test_backspace_in_middle() {
        let mut input = TextInputState::with_value("hello");
        input.cursor = 3;
        input.backspace();
        assert_eq!(input.value(), "helo");
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn test_delete() {
        let mut input = TextInputState::with_value("hello");
        input.cursor = 0;
        input.delete();
        assert_eq!(input.value(), "ello");
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn test_delete_at_end() {
        let mut input = TextInputState::with_value("hello");
        input.delete();
        assert_eq!(input.value(), "hello");
        assert_eq!(input.cursor(), 5);
    }

    #[test]
    fn test_move_left() {
        let mut input = TextInputState::with_value("hello");
        input.move_left();
        assert_eq!(input.cursor(), 4);
        input.move_left();
        assert_eq!(input.cursor(), 3);
    }

    #[test]
    fn test_move_left_at_start() {
        let mut input = TextInputState::with_value("hello");
        input.cursor = 0;
        input.move_left();
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn test_move_right() {
        let mut input = TextInputState::with_value("hello");
        input.cursor = 0;
        input.move_right();
        assert_eq!(input.cursor(), 1);
        input.move_right();
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn test_move_right_at_end() {
        let mut input = TextInputState::with_value("hello");
        input.move_right();
        assert_eq!(input.cursor(), 5);
    }

    #[test]
    fn test_home() {
        let mut input = TextInputState::with_value("hello");
        input.home();
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn test_end() {
        let mut input = TextInputState::with_value("hello");
        input.cursor = 2;
        input.end();
        assert_eq!(input.cursor(), 5);
    }

    #[test]
    fn test_clear() {
        let mut input = TextInputState::with_value("hello");
        input.clear();
        assert!(input.is_empty());
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn test_before_after_cursor() {
        let mut input = TextInputState::with_value("hello");
        input.cursor = 2;
        assert_eq!(input.before_cursor(), "he");
        assert_eq!(input.after_cursor(), "llo");
    }

    #[test]
    fn test_take() {
        let input = TextInputState::with_value("hello");
        let value = input.take();
        assert_eq!(value, "hello");
    }

    #[test]
    fn test_set_value() {
        let mut input = TextInputState::with_value("hello");
        input.set_value("world");
        assert_eq!(input.value(), "world");
        assert_eq!(input.cursor(), 5);
    }

    #[test]
    fn test_unicode_support() {
        let mut input = TextInputState::with_value("hello");
        input.cursor = 2;
        input.insert('\u{00e9}'); // é
        assert_eq!(input.value(), "heéllo");

        // Backspace should remove the multi-byte char
        input.backspace();
        assert_eq!(input.value(), "hello");
    }
}
