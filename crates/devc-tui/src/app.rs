//! Main TUI application state and logic

use crate::event::{Event, EventHandler};
use crate::ui;
use crossterm::event::{KeyCode, KeyModifiers};
use devc_core::{ContainerManager, ContainerState, DevcContainerStatus};
use ratatui::prelude::*;
use std::time::Duration;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Core error: {0}")]
    Core(#[from] devc_core::CoreError),
}

pub type AppResult<T> = Result<T, AppError>;

/// Current view in the application
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Main dashboard with container list
    Dashboard,
    /// Detailed view of a single container
    ContainerDetail,
    /// Build output view
    BuildOutput,
    /// Container logs view
    Logs,
    /// Help view
    Help,
    /// Confirmation dialog
    Confirm,
}

/// Confirmation action
#[derive(Debug, Clone)]
pub enum ConfirmAction {
    Delete(String),
    Stop(String),
}

/// Application state
pub struct App {
    /// Container manager
    pub manager: ContainerManager,
    /// Current view
    pub view: View,
    /// List of containers
    pub containers: Vec<ContainerState>,
    /// Currently selected container index
    pub selected: usize,
    /// Build output log
    pub build_output: Vec<String>,
    /// Container logs
    pub logs: Vec<String>,
    /// Logs scroll position (line offset from top)
    pub logs_scroll: usize,
    /// Status message
    pub status_message: Option<String>,
    /// Should quit
    pub should_quit: bool,
    /// Pending confirmation action
    pub confirm_action: Option<ConfirmAction>,
    /// Is an operation in progress
    pub loading: bool,
}

impl App {
    /// Create a new application
    pub async fn new(manager: ContainerManager) -> AppResult<Self> {
        let containers = manager.list().await?;

        Ok(Self {
            manager,
            view: View::Dashboard,
            containers,
            selected: 0,
            build_output: Vec::new(),
            logs: Vec::new(),
            logs_scroll: 0,
            status_message: None,
            should_quit: false,
            confirm_action: None,
            loading: false,
        })
    }

    /// Run the application main loop
    pub async fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> AppResult<()> {
        let mut events = EventHandler::new(Duration::from_millis(250));

        while !self.should_quit {
            // Draw UI
            terminal.draw(|frame| ui::draw(frame, self))?;

            // Handle events
            if let Some(event) = events.next().await {
                self.handle_event(event).await?;
            }
        }

        Ok(())
    }

    /// Handle an event
    async fn handle_event(&mut self, event: Event) -> AppResult<()> {
        match event {
            Event::Key(key) => {
                self.handle_key(key.code, key.modifiers).await?;
            }
            Event::Tick => {
                // Refresh container list periodically
                self.refresh_containers().await?;
            }
            Event::Resize(_, _) => {
                // Terminal will redraw automatically
            }
            Event::Mouse(_) => {
                // Mouse events not handled yet
            }
        }
        Ok(())
    }

    /// Handle key press
    async fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> AppResult<()> {
        // Handle confirmation dialog
        if self.view == View::Confirm {
            match code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(action) = self.confirm_action.take() {
                        self.execute_confirm_action(action).await?;
                    }
                    self.view = View::Dashboard;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirm_action = None;
                    self.view = View::Dashboard;
                }
                _ => {}
            }
            return Ok(());
        }

        // Global keys
        match code {
            KeyCode::Char('q') => {
                if self.view == View::Dashboard {
                    self.should_quit = true;
                } else {
                    self.view = View::Dashboard;
                }
            }
            KeyCode::Char('?') => {
                self.view = View::Help;
            }
            KeyCode::Esc => {
                if self.view != View::Dashboard {
                    self.view = View::Dashboard;
                }
            }
            _ => {}
        }

        // View-specific keys
        match self.view {
            View::Dashboard => self.handle_dashboard_key(code, modifiers).await?,
            View::ContainerDetail => self.handle_detail_key(code, modifiers).await?,
            View::BuildOutput => self.handle_build_key(code, modifiers).await?,
            View::Logs => self.handle_logs_key(code, modifiers).await?,
            View::Help => {
                // Any key returns to previous view
                if code != KeyCode::Char('?') {
                    self.view = View::Dashboard;
                }
            }
            View::Confirm => {} // Handled above
        }

        Ok(())
    }

    /// Handle dashboard view keys
    async fn handle_dashboard_key(
        &mut self,
        code: KeyCode,
        _modifiers: KeyModifiers,
    ) -> AppResult<()> {
        match code {
            // Navigation
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.containers.is_empty() {
                    self.selected = (self.selected + 1) % self.containers.len();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if !self.containers.is_empty() {
                    self.selected = self.selected.checked_sub(1).unwrap_or(self.containers.len() - 1);
                }
            }
            KeyCode::Char('g') => {
                self.selected = 0;
            }
            KeyCode::Char('G') => {
                if !self.containers.is_empty() {
                    self.selected = self.containers.len() - 1;
                }
            }

            // Actions
            KeyCode::Enter => {
                if !self.containers.is_empty() {
                    self.view = View::ContainerDetail;
                }
            }
            KeyCode::Char('b') => {
                self.build_selected().await?;
            }
            KeyCode::Char('s') => {
                self.toggle_selected().await?;
            }
            KeyCode::Char('u') => {
                self.up_selected().await?;
            }
            KeyCode::Char('d') => {
                if !self.containers.is_empty() {
                    let container = &self.containers[self.selected];
                    self.confirm_action = Some(ConfirmAction::Delete(container.id.clone()));
                    self.view = View::Confirm;
                }
            }
            KeyCode::Char('r') => {
                self.refresh_containers().await?;
            }

            _ => {}
        }
        Ok(())
    }

    /// Handle container detail view keys
    async fn handle_detail_key(
        &mut self,
        code: KeyCode,
        _modifiers: KeyModifiers,
    ) -> AppResult<()> {
        match code {
            KeyCode::Char('b') => {
                self.build_selected().await?;
            }
            KeyCode::Char('s') => {
                self.toggle_selected().await?;
            }
            KeyCode::Char('u') => {
                self.up_selected().await?;
            }
            KeyCode::Char('l') => {
                self.fetch_logs().await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Handle build output view keys
    async fn handle_build_key(
        &mut self,
        code: KeyCode,
        _modifiers: KeyModifiers,
    ) -> AppResult<()> {
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                // Scroll down
            }
            KeyCode::Char('k') | KeyCode::Up => {
                // Scroll up
            }
            _ => {}
        }
        Ok(())
    }

    /// Handle logs view keys with vim-like navigation
    async fn handle_logs_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> AppResult<()> {
        // Calculate visible lines (approximate, will be refined by UI)
        let page_size = 20;

        match code {
            // Single line movement
            KeyCode::Char('j') | KeyCode::Down => {
                if self.logs_scroll < self.logs.len().saturating_sub(1) {
                    self.logs_scroll += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.logs_scroll = self.logs_scroll.saturating_sub(1);
            }
            // Go to top/bottom
            KeyCode::Char('g') => {
                self.logs_scroll = 0;
            }
            KeyCode::Char('G') => {
                self.logs_scroll = self.logs.len().saturating_sub(1);
            }
            // Half page movement (Ctrl+D / Ctrl+U)
            KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.logs_scroll = (self.logs_scroll + page_size / 2)
                    .min(self.logs.len().saturating_sub(1));
            }
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.logs_scroll = self.logs_scroll.saturating_sub(page_size / 2);
            }
            // Full page movement (Ctrl+F / Ctrl+B)
            KeyCode::Char('f') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.logs_scroll = (self.logs_scroll + page_size)
                    .min(self.logs.len().saturating_sub(1));
            }
            KeyCode::Char('b') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.logs_scroll = self.logs_scroll.saturating_sub(page_size);
            }
            // Refresh logs
            KeyCode::Char('r') => {
                self.fetch_logs().await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Refresh container list
    async fn refresh_containers(&mut self) -> AppResult<()> {
        self.containers = self.manager.list().await?;

        // Sync status for all containers
        for container in &self.containers {
            let _ = self.manager.sync_status(&container.id).await;
        }

        // Re-fetch after sync
        self.containers = self.manager.list().await?;

        // Ensure selected index is valid
        if !self.containers.is_empty() && self.selected >= self.containers.len() {
            self.selected = self.containers.len() - 1;
        }

        Ok(())
    }

    /// Build selected container
    async fn build_selected(&mut self) -> AppResult<()> {
        if self.containers.is_empty() {
            return Ok(());
        }

        let container = &self.containers[self.selected];
        self.status_message = Some(format!("Building {}...", container.name));
        self.loading = true;
        self.view = View::BuildOutput;
        self.build_output.clear();
        self.build_output.push(format!("Building container: {}", container.name));

        match self.manager.build(&container.id).await {
            Ok(image_id) => {
                self.build_output.push(format!("Built image: {}", image_id));
                self.status_message = Some(format!("Built {}", container.name));
            }
            Err(e) => {
                self.build_output.push(format!("Build failed: {}", e));
                self.status_message = Some(format!("Build failed: {}", e));
            }
        }

        self.loading = false;
        self.refresh_containers().await?;
        Ok(())
    }

    /// Toggle start/stop for selected container
    async fn toggle_selected(&mut self) -> AppResult<()> {
        if self.containers.is_empty() {
            return Ok(());
        }

        let container = &self.containers[self.selected];
        self.loading = true;

        match container.status {
            DevcContainerStatus::Running => {
                self.status_message = Some(format!("Stopping {}...", container.name));
                match self.manager.stop(&container.id).await {
                    Ok(()) => {
                        self.status_message = Some(format!("Stopped {}", container.name));
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Stop failed: {}", e));
                    }
                }
            }
            DevcContainerStatus::Stopped | DevcContainerStatus::Created => {
                self.status_message = Some(format!("Starting {}...", container.name));
                match self.manager.start(&container.id).await {
                    Ok(()) => {
                        self.status_message = Some(format!("Started {}", container.name));
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Start failed: {}", e));
                    }
                }
            }
            _ => {
                self.status_message = Some("Cannot start/stop in current state".to_string());
            }
        }

        self.loading = false;
        self.refresh_containers().await?;
        Ok(())
    }

    /// Run full up (build, create, start) for selected container
    async fn up_selected(&mut self) -> AppResult<()> {
        if self.containers.is_empty() {
            return Ok(());
        }

        let container = &self.containers[self.selected];
        self.status_message = Some(format!("Starting {}...", container.name));
        self.loading = true;
        self.view = View::BuildOutput;
        self.build_output.clear();
        self.build_output.push(format!("Starting container: {}", container.name));

        match self.manager.up(&container.id).await {
            Ok(()) => {
                self.build_output.push("Container is running".to_string());
                self.status_message = Some(format!("{} is running", container.name));
            }
            Err(e) => {
                self.build_output.push(format!("Failed: {}", e));
                self.status_message = Some(format!("Failed: {}", e));
            }
        }

        self.loading = false;
        self.refresh_containers().await?;
        Ok(())
    }

    /// Fetch logs for the selected container
    async fn fetch_logs(&mut self) -> AppResult<()> {
        if self.containers.is_empty() {
            return Ok(());
        }

        let container = &self.containers[self.selected];

        // Only fetch logs if container has been created
        if container.container_id.is_none() {
            self.status_message = Some("Container has not been created yet".to_string());
            return Ok(());
        }

        self.status_message = Some(format!("Loading logs for {}...", container.name));
        self.loading = true;

        match self.manager.logs(&container.id, Some(1000)).await {
            Ok(lines) => {
                self.logs = lines;
                self.logs_scroll = self.logs.len().saturating_sub(1); // Start at bottom
                self.view = View::Logs;
                self.status_message = Some(format!("{} log lines", self.logs.len()));
            }
            Err(e) => {
                self.status_message = Some(format!("Failed to fetch logs: {}", e));
            }
        }

        self.loading = false;
        Ok(())
    }

    /// Execute a confirmed action
    async fn execute_confirm_action(&mut self, action: ConfirmAction) -> AppResult<()> {
        match action {
            ConfirmAction::Delete(id) => {
                self.loading = true;
                match self.manager.remove(&id, true).await {
                    Ok(()) => {
                        self.status_message = Some("Container deleted".to_string());
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Delete failed: {}", e));
                    }
                }
                self.loading = false;
                self.refresh_containers().await?;
            }
            ConfirmAction::Stop(id) => {
                self.loading = true;
                match self.manager.stop(&id).await {
                    Ok(()) => {
                        self.status_message = Some("Container stopped".to_string());
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Stop failed: {}", e));
                    }
                }
                self.loading = false;
                self.refresh_containers().await?;
            }
        }
        Ok(())
    }

    /// Get the currently selected container
    pub fn selected_container(&self) -> Option<&ContainerState> {
        self.containers.get(self.selected)
    }
}
