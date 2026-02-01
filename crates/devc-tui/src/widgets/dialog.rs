//! Reusable dialog builder widget
//!
//! Provides a builder pattern for creating modal dialogs with consistent styling.

use crate::app::DialogFocus;
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, Paragraph},
};

/// Builder for creating modal dialogs
pub struct DialogBuilder<'a> {
    title: &'a str,
    lines: Vec<Line<'a>>,
    width: u16,
    border_color: Color,
}

impl<'a> DialogBuilder<'a> {
    /// Create a new dialog builder with a title
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            lines: Vec::new(),
            width: 50,
            border_color: Color::Yellow,
        }
    }

    /// Set the dialog width
    pub fn width(mut self, w: u16) -> Self {
        self.width = w;
        self
    }

    /// Set the border color
    pub fn border_color(mut self, color: Color) -> Self {
        self.border_color = color;
        self
    }

    /// Add a message line
    pub fn message(mut self, text: &'a str) -> Self {
        self.lines.push(Line::from(text.to_string()));
        self
    }

    /// Add a styled message line
    pub fn styled_message(mut self, line: Line<'a>) -> Self {
        self.lines.push(line);
        self
    }

    /// Add an empty line for spacing
    pub fn empty_line(mut self) -> Self {
        self.lines.push(Line::from(""));
        self
    }

    /// Add Yes/No buttons with focus highlighting
    pub fn buttons(mut self, focus: DialogFocus) -> Self {
        let confirm_style = if focus == DialogFocus::Confirm {
            Style::default().bg(Color::Green).fg(Color::Black).bold()
        } else {
            Style::default().fg(Color::Green)
        };
        let cancel_style = if focus == DialogFocus::Cancel {
            Style::default().bg(Color::Red).fg(Color::White).bold()
        } else {
            Style::default().fg(Color::Red)
        };

        self.lines.push(Line::from(vec![
            Span::styled("  Confirm  ", confirm_style),
            Span::raw("    "),
            Span::styled("  Cancel  ", cancel_style),
        ]));
        self
    }

    /// Add a checkbox with label
    pub fn checkbox(mut self, label: &'a str, checked: bool, focused: bool) -> Self {
        let checkbox_str = if checked { "[X]" } else { "[ ]" };
        let focus_indicator = if focused { "\u{25B6} " } else { "  " }; // ▶

        let checkbox_style = if focused {
            Style::default().bg(Color::Cyan).fg(Color::Black).bold()
        } else {
            Style::default().fg(Color::Cyan)
        };

        let label_style = if focused {
            Style::default().bold()
        } else {
            Style::default()
        };

        self.lines.push(Line::from(vec![
            Span::raw(focus_indicator),
            Span::styled(checkbox_str, checkbox_style),
            Span::styled(format!(" {}", label), label_style),
        ]));
        self
    }

    /// Add help text at the bottom
    pub fn help(mut self, text: &'a str) -> Self {
        self.lines.push(Line::from(Span::styled(
            text.to_string(),
            Style::default().fg(Color::DarkGray),
        )));
        self
    }

    /// Render the dialog centered in the given area
    pub fn render(self, frame: &mut Frame, area: Rect) {
        let height = (self.lines.len() as u16) + 2; // +2 for borders
        let dialog_area = centered_rect(self.width, height, area);

        frame.render_widget(Clear, dialog_area);

        let dialog = Paragraph::new(self.lines)
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .title(format!(" {} ", self.title))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(self.border_color)),
            );

        frame.render_widget(dialog, dialog_area);
    }
}

/// Calculate a centered rectangle within an area
pub fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    Rect {
        x: (area.width.saturating_sub(width)) / 2,
        y: (area.height.saturating_sub(height)) / 2,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_centered_rect() {
        let area = Rect::new(0, 0, 100, 50);
        let centered = centered_rect(40, 20, area);

        assert_eq!(centered.x, 30);
        assert_eq!(centered.y, 15);
        assert_eq!(centered.width, 40);
        assert_eq!(centered.height, 20);
    }

    #[test]
    fn test_centered_rect_overflow() {
        let area = Rect::new(0, 0, 30, 20);
        let centered = centered_rect(50, 30, area);

        // Should be clamped to area size
        assert_eq!(centered.width, 30);
        assert_eq!(centered.height, 20);
    }

    #[test]
    fn test_dialog_builder_chain() {
        let builder = DialogBuilder::new("Test")
            .width(60)
            .border_color(Color::Cyan)
            .empty_line()
            .message("Hello, World!")
            .empty_line();

        assert_eq!(builder.width, 60);
        assert_eq!(builder.border_color, Color::Cyan);
        assert_eq!(builder.lines.len(), 3);
    }
}
