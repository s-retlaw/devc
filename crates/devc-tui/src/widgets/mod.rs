//! Reusable widget abstractions for the TUI

mod dialog;
mod text_input;

pub use dialog::{centered_rect, DialogBuilder};
pub use text_input::TextInputState;
