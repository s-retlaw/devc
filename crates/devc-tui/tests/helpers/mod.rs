use devc_tui::App;
use ratatui::{backend::TestBackend, Terminal};

/// Render the app to a TestBackend and capture output as a string
#[allow(dead_code)]
pub fn render_app(app: &mut App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| devc_tui::ui::draw(frame, app))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    buffer_to_string(&buffer)
}

/// Convert a ratatui buffer to a string representation
#[allow(dead_code)]
pub fn buffer_to_string(buffer: &ratatui::buffer::Buffer) -> String {
    let mut output = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            let cell = buffer.get(x, y);
            output.push_str(cell.symbol());
        }
        output.push('\n');
    }
    output
}
