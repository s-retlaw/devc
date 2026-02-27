pub const DOTS_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn frame(index: usize) -> &'static str {
    DOTS_SPINNER[index % DOTS_SPINNER.len()]
}
