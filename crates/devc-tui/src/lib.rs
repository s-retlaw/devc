//! TUI application for devc
//!
//! Built with Ratatui for a modern terminal UI experience.

pub mod app;
mod clipboard;
mod demo;
mod event;
pub mod ports;
pub mod settings;
pub mod shell;
pub mod stats;
pub mod tunnel;
pub mod ui;
pub mod widgets;

pub use clipboard::copy_to_clipboard;

pub use app::{App, AppResult, ConfirmAction, ContainerOperation, DialogFocus, ShellSession, Tab, View};
pub use demo::DemoApp;
pub use event::{Event, EventHandler};
pub use shell::{reset_terminal, ShellConfig, ShellExitReason};
#[cfg(unix)]
pub use shell::PtyShell;

use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use devc_core::ContainerManager;
use ratatui::prelude::*;
use std::io::{self, Write};
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

/// Suspend TUI mode for shell access
///
/// Leaves alternate screen and disables raw mode so the shell process
/// can have direct terminal control with proper PTY support.
///
/// IMPORTANT: Order matters! Leave alternate screen BEFORE disabling raw mode.
/// This ensures the shell inherits a clean terminal state without corruption.
pub fn suspend_tui(stdout: &mut impl Write) -> io::Result<()> {
    execute!(stdout, LeaveAlternateScreen, DisableMouseCapture)?;
    disable_raw_mode()?;
    stdout.flush()?;
    Ok(())
}

/// Resume TUI mode after shell exit
///
/// Re-enables raw mode and enters alternate screen to restore
/// the TUI display.
pub fn resume_tui(stdout: &mut impl Write) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    stdout.flush()?;
    Ok(())
}
