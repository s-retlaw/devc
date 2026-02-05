//! TUI application for devc
//!
//! Built with Ratatui for a modern terminal UI experience.

pub mod app;
mod clipboard;
mod demo;
mod event;
pub mod ports;
pub mod settings;
pub mod tunnel;
pub mod ui;
pub mod widgets;

pub use clipboard::copy_to_clipboard;

pub use app::{App, AppResult, ConfirmAction, DialogFocus, Tab, View};
pub use demo::DemoApp;
pub use event::{Event, EventHandler};

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use devc_core::ContainerManager;
use ratatui::prelude::*;
use std::io;
use tracing_subscriber::layer::SubscriberExt;

/// Run the TUI application
pub async fn run(manager: ContainerManager) -> AppResult<()> {
    // Suppress tracing output during TUI (use a no-op subscriber to prevent logs from corrupting display)
    // The guard restores the previous subscriber when dropped
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::registry().with(tracing_subscriber::layer::Identity::new()),
    );

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app and run
    let mut app = App::new(manager).await?;
    let res = app.run(&mut terminal).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    res
}

/// Run the TUI in demo mode with mock data
pub async fn run_demo() -> AppResult<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create demo app and run
    let mut app = DemoApp::new();
    let res = app.run(&mut terminal).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    res
}
