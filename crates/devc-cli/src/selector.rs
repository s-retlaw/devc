//! Interactive container selector for CLI commands

use anyhow::{bail, Result};
use crossterm::{
    cursor::{self, MoveToColumn, MoveUp},
    event::{self, Event, KeyCode, KeyEvent},
    style::{Color, ResetColor, SetForegroundColor},
    terminal::{self, Clear, ClearType},
    ExecutableCommand,
};
use devc_core::{ContainerState, DevcContainerStatus};
use std::io::{stdout, Write};

/// Context for filtering containers in the selector
#[derive(Debug, Clone, Copy)]
pub enum SelectionContext {
    /// Only running containers (for shell, run, stop)
    Running,
    /// Startable containers: Stopped, Built, Created (for start)
    Startable,
    /// Non-running containers for `up` command
    Uppable,
    /// All containers (for down, rm)
    Any,
}

impl SelectionContext {
    /// Filter containers based on selection context
    pub fn filter(&self, containers: &[ContainerState]) -> Vec<ContainerState> {
        containers
            .iter()
            .filter(|c| self.matches(c))
            .cloned()
            .collect()
    }

    /// Check if a container matches this selection context
    fn matches(&self, container: &ContainerState) -> bool {
        match self {
            SelectionContext::Running => container.status == DevcContainerStatus::Running,
            SelectionContext::Startable => matches!(
                container.status,
                DevcContainerStatus::Stopped
                    | DevcContainerStatus::Built
                    | DevcContainerStatus::Created
            ),
            SelectionContext::Uppable => container.status != DevcContainerStatus::Running,
            SelectionContext::Any => true,
        }
    }

    /// Get a description for the empty state message
    fn description(&self) -> &'static str {
        match self {
            SelectionContext::Running => "running",
            SelectionContext::Startable => "startable",
            SelectionContext::Uppable => "non-running",
            SelectionContext::Any => "available",
        }
    }
}

/// Guard that restores terminal state on drop
struct RawModeGuard {
    was_raw: bool,
}

impl RawModeGuard {
    fn new() -> Result<Self> {
        let was_raw = terminal::is_raw_mode_enabled()?;
        if !was_raw {
            terminal::enable_raw_mode()?;
        }
        Ok(Self { was_raw })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if !self.was_raw {
            let _ = terminal::disable_raw_mode();
        }
    }
}

/// Get status symbol for a container
fn status_symbol(status: &DevcContainerStatus) -> &'static str {
    match status {
        DevcContainerStatus::Running => "●",
        DevcContainerStatus::Stopped => "○",
        DevcContainerStatus::Building => "◐",
        DevcContainerStatus::Built => "◑",
        DevcContainerStatus::Created => "◔",
        DevcContainerStatus::Failed => "✗",
        DevcContainerStatus::Configured => "◯",
    }
}

/// Get color for a status
fn status_color(status: &DevcContainerStatus) -> Color {
    match status {
        DevcContainerStatus::Running => Color::Green,
        DevcContainerStatus::Stopped => Color::DarkGrey,
        DevcContainerStatus::Building => Color::Yellow,
        DevcContainerStatus::Built => Color::Cyan,
        DevcContainerStatus::Created => Color::Blue,
        DevcContainerStatus::Failed => Color::Red,
        DevcContainerStatus::Configured => Color::DarkGrey,
    }
}

/// Interactively select a container from the list
///
/// Returns the selected container's name, or an error if cancelled or no containers available.
pub fn select_container(
    containers: &[ContainerState],
    context: SelectionContext,
    prompt: &str,
) -> Result<String> {
    // Check if we're in a TTY
    if !std::io::stdin().is_terminal() {
        bail!("Cannot show interactive selector: not a TTY. Specify container name as argument.");
    }

    // Filter containers based on context
    let filtered = context.filter(containers);

    if filtered.is_empty() {
        bail!(
            "No {} containers found. Use 'devc list' to see all containers.",
            context.description()
        );
    }

    // Enable raw mode for keyboard input
    let _guard = RawModeGuard::new()?;
    let mut stdout = stdout();

    let mut selected: usize = 0;
    let total = filtered.len();

    // Initial render
    render_selector(&mut stdout, &filtered, selected, prompt)?;

    // Event loop
    loop {
        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(KeyEvent { code, .. }) = event::read()? {
                match code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        if selected > 0 {
                            selected -= 1;
                        } else {
                            selected = total - 1; // Wrap to bottom
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if selected < total - 1 {
                            selected += 1;
                        } else {
                            selected = 0; // Wrap to top
                        }
                    }
                    KeyCode::Enter => {
                        // Clear the selector UI
                        clear_selector(&mut stdout, total)?;
                        return Ok(filtered[selected].name.clone());
                    }
                    KeyCode::Esc | KeyCode::Char('q') => {
                        // Clear the selector UI
                        clear_selector(&mut stdout, total)?;
                        bail!("Selection cancelled");
                    }
                    _ => {}
                }

                // Re-render after key press
                rerender_selector(&mut stdout, &filtered, selected, total)?;
            }
        }
    }
}

/// Render the selector UI
fn render_selector(
    stdout: &mut std::io::Stdout,
    containers: &[ContainerState],
    selected: usize,
    prompt: &str,
) -> Result<()> {
    // Hide cursor during selection
    stdout.execute(cursor::Hide)?;

    // Print prompt (use \r\n in raw mode for proper line breaks)
    write!(stdout, "{}\r\n", prompt)?;

    // Print each container option
    for (i, container) in containers.iter().enumerate() {
        render_line(stdout, container, i == selected)?;
    }

    // Print help line
    write!(stdout, "\r\n")?;
    stdout.execute(SetForegroundColor(Color::DarkGrey))?;
    write!(stdout, "[↑/↓ or j/k to move, Enter to select, Esc to cancel]")?;
    stdout.execute(ResetColor)?;
    stdout.flush()?;

    Ok(())
}

/// Render a single container line
fn render_line(
    stdout: &mut std::io::Stdout,
    container: &ContainerState,
    is_selected: bool,
) -> Result<()> {
    if is_selected {
        stdout.execute(SetForegroundColor(Color::White))?;
        write!(stdout, "> ")?;
    } else {
        write!(stdout, "  ")?;
    }

    // Status symbol with color
    stdout.execute(SetForegroundColor(status_color(&container.status)))?;
    write!(stdout, "{}", status_symbol(&container.status))?;
    stdout.execute(ResetColor)?;

    // Container name
    if is_selected {
        stdout.execute(SetForegroundColor(Color::White))?;
    }
    write!(stdout, " {:<24}", container.name)?;

    // Status text
    stdout.execute(SetForegroundColor(Color::DarkGrey))?;
    write!(stdout, "{}", container.status)?;
    stdout.execute(ResetColor)?;

    // Use \r\n in raw mode for proper line breaks
    write!(stdout, "\r\n")?;
    Ok(())
}

/// Re-render the selector after a key press (moves cursor up and redraws)
fn rerender_selector(
    stdout: &mut std::io::Stdout,
    containers: &[ContainerState],
    selected: usize,
    total: usize,
) -> Result<()> {
    // Move up from help line to first container line:
    // help -> empty (1) -> containers (total) = total + 1 lines
    let lines_to_move = total + 1;
    stdout.execute(MoveUp(lines_to_move as u16))?;
    stdout.execute(MoveToColumn(0))?;

    // Redraw each container line
    for (i, container) in containers.iter().enumerate() {
        stdout.execute(Clear(ClearType::CurrentLine))?;
        render_line(stdout, container, i == selected)?;
    }

    // Redraw empty line and help text
    stdout.execute(Clear(ClearType::CurrentLine))?;
    write!(stdout, "\r\n")?;
    stdout.execute(Clear(ClearType::CurrentLine))?;
    stdout.execute(SetForegroundColor(Color::DarkGrey))?;
    write!(stdout, "[↑/↓ or j/k to move, Enter to select, Esc to cancel]")?;
    stdout.execute(ResetColor)?;
    stdout.flush()?;

    Ok(())
}

/// Clear the selector UI and show cursor
fn clear_selector(stdout: &mut std::io::Stdout, total: usize) -> Result<()> {
    // Layout: prompt (1) + containers (total) + empty (1) + help (1, cursor here)
    // Move up to prompt: total + 2 lines
    let lines_to_move = total + 2;
    stdout.execute(MoveUp(lines_to_move as u16))?;
    stdout.execute(MoveToColumn(0))?;

    // Clear all lines (prompt + containers + empty + help = total + 3 lines)
    for _ in 0..(total + 3) {
        stdout.execute(Clear(ClearType::CurrentLine))?;
        write!(stdout, "\r\n")?;
    }

    // Move back up to where prompt was
    stdout.execute(MoveUp((total + 3) as u16))?;
    stdout.execute(MoveToColumn(0))?;

    // Show cursor again
    stdout.execute(cursor::Show)?;
    stdout.flush()?;

    Ok(())
}

/// Check if stdin is a terminal
trait IsTerminal {
    fn is_terminal(&self) -> bool;
}

impl IsTerminal for std::io::Stdin {
    fn is_terminal(&self) -> bool {
        std::io::IsTerminal::is_terminal(self)
    }
}
