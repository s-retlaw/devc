//! Main TUI application state and logic

use crate::event::{Event, EventHandler};
use crate::settings::{ProviderDetailState, SettingsState};
use crate::ui;
use crossterm::event::{KeyCode, KeyModifiers};
use devc_config::GlobalConfig;
use devc_core::{ContainerManager, ContainerState, DevcContainerStatus};
use devc_provider::ProviderType;
use ratatui::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Core error: {0}")]
    Core(#[from] devc_core::CoreError),
}

pub type AppResult<T> = Result<T, AppError>;

/// Main tab in the application (always visible at top)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Containers,
    Providers,
    Settings,
}

impl Tab {
    pub fn all() -> &'static [Tab] {
        &[Tab::Containers, Tab::Providers, Tab::Settings]
    }

    pub fn label(&self) -> &'static str {
        match self {
            Tab::Containers => "Containers",
            Tab::Providers => "Providers",
            Tab::Settings => "Settings",
        }
    }

    pub fn index(&self) -> usize {
        match self {
            Tab::Containers => 0,
            Tab::Providers => 1,
            Tab::Settings => 2,
        }
    }
}

/// Current view/subview in the application
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Main view (depends on current tab)
    Main,
    /// Detailed view of a single container
    ContainerDetail,
    /// Provider detail/configuration view
    ProviderDetail,
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
    Rebuild {
        id: String,
        provider_change: Option<(ProviderType, ProviderType)>, // (old, new)
    },
}

/// Provider status information
#[derive(Debug, Clone)]
pub struct ProviderStatus {
    pub provider_type: ProviderType,
    pub name: String,
    pub socket: String,
    pub connected: bool,
    pub is_active: bool,
}

/// Application state
pub struct App {
    /// Container manager (wrapped in Arc for sharing with background tasks)
    pub manager: Arc<ContainerManager>,
    /// Global configuration
    pub config: GlobalConfig,
    /// Current tab
    pub tab: Tab,
    /// Current view within the tab
    pub view: View,
    /// Active provider type (for new containers)
    pub active_provider: ProviderType,
    /// Provider statuses
    pub providers: Vec<ProviderStatus>,
    /// Selected provider index (in Providers tab)
    pub selected_provider: usize,
    /// List of containers
    pub containers: Vec<ContainerState>,
    /// Currently selected container index
    pub selected: usize,
    /// Build output log
    pub build_output: Vec<String>,
    /// Build output scroll position
    pub build_output_scroll: usize,
    /// Auto-scroll to bottom when new build output arrives
    pub build_auto_scroll: bool,
    /// Whether the build has completed (success or error)
    pub build_complete: bool,
    /// Channel receiver for build progress updates
    pub build_progress_rx: Option<mpsc::UnboundedReceiver<String>>,
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
    /// Rebuild no-cache toggle state (for rebuild confirmation dialog)
    pub rebuild_no_cache: bool,
    /// Settings state (for global settings)
    pub settings_state: SettingsState,
    /// Provider detail state (for provider-specific settings)
    pub provider_detail_state: ProviderDetailState,
}

impl App {
    /// Create a new application
    pub async fn new(manager: ContainerManager) -> AppResult<Self> {
        let containers = manager.list().await?;
        let config = GlobalConfig::load().unwrap_or_default();
        let active_provider = manager.provider_type();
        let settings_state = SettingsState::new(&config);

        // Build provider status list
        let providers = vec![
            ProviderStatus {
                provider_type: ProviderType::Podman,
                name: "Podman".to_string(),
                socket: config.providers.podman.socket.clone(),
                connected: active_provider == ProviderType::Podman,
                is_active: active_provider == ProviderType::Podman,
            },
            ProviderStatus {
                provider_type: ProviderType::Docker,
                name: "Docker".to_string(),
                socket: config.providers.docker.socket.clone(),
                connected: active_provider == ProviderType::Docker,
                is_active: active_provider == ProviderType::Docker,
            },
        ];

        Ok(Self {
            manager: Arc::new(manager),
            config,
            tab: Tab::Containers,
            view: View::Main,
            active_provider,
            providers,
            selected_provider: if active_provider == ProviderType::Podman { 0 } else { 1 },
            containers,
            selected: 0,
            build_output: Vec::new(),
            build_output_scroll: 0,
            build_auto_scroll: true,
            build_complete: false,
            build_progress_rx: None,
            logs: Vec::new(),
            logs_scroll: 0,
            status_message: None,
            should_quit: false,
            confirm_action: None,
            loading: false,
            rebuild_no_cache: false,
            settings_state,
            provider_detail_state: ProviderDetailState::new(),
        })
    }

    /// Run the application main loop
    pub async fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> AppResult<()> {
        let mut events = EventHandler::new(Duration::from_millis(250));

        while !self.should_quit {
            // Draw UI
            terminal.draw(|frame| ui::draw(frame, self))?;

            // Use select to handle multiple event sources for immediate updates
            tokio::select! {
                // Terminal/keyboard events
                event = events.next() => {
                    if let Some(e) = event {
                        self.handle_event(e).await?;
                    }
                }
                // Build progress updates (immediate, no tick delay)
                progress = Self::recv_progress(&mut self.build_progress_rx) => {
                    if let Some(line) = progress {
                        self.handle_build_progress(line).await?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Helper to receive from optional channel
    async fn recv_progress(rx: &mut Option<mpsc::UnboundedReceiver<String>>) -> Option<String> {
        match rx {
            Some(ref mut receiver) => receiver.recv().await,
            None => std::future::pending().await,
        }
    }

    /// Handle a single build progress message
    async fn handle_build_progress(&mut self, line: String) -> AppResult<()> {
        let is_complete = line.contains("complete") || line.contains("Error:");
        self.build_output.push(line);

        if is_complete {
            self.loading = false;
            self.build_complete = true;
            self.build_progress_rx = None;
            self.refresh_containers().await?;
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
                // Refresh container list periodically (only on Containers tab main view)
                if self.tab == Tab::Containers && self.view == View::Main && !self.loading {
                    self.refresh_containers().await?;
                }
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
        // Handle confirmation dialog first
        if self.view == View::Confirm {
            match code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    if let Some(action) = self.confirm_action.take() {
                        self.execute_confirm_action(action).await?;
                    }
                    // Only return to Main if the action didn't change the view
                    // (Rebuild changes to BuildOutput)
                    if self.view == View::Confirm {
                        self.view = View::Main;
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirm_action = None;
                    self.rebuild_no_cache = false;
                    self.view = View::Main;
                }
                KeyCode::Char(' ') => {
                    // Toggle no_cache option if this is a rebuild action
                    if matches!(self.confirm_action, Some(ConfirmAction::Rebuild { .. })) {
                        self.rebuild_no_cache = !self.rebuild_no_cache;
                    }
                }
                _ => {}
            }
            return Ok(());
        }

        // Handle Help view
        if self.view == View::Help {
            // Any key returns to main view
            self.view = View::Main;
            return Ok(());
        }

        // Global keys (work in any view)
        match code {
            KeyCode::Char('q') => {
                // Don't close BuildOutput view during active build
                if self.view == View::BuildOutput && !self.build_complete {
                    return Ok(());
                }
                if self.view != View::Main {
                    self.view = View::Main;
                } else {
                    self.should_quit = true;
                }
                return Ok(());
            }
            KeyCode::Esc => {
                // Don't close BuildOutput view during active build
                if self.view == View::BuildOutput && !self.build_complete {
                    return Ok(());
                }
                if self.view != View::Main {
                    self.view = View::Main;
                }
                return Ok(());
            }
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.view = View::Help;
                return Ok(());
            }
            // Tab switching with number keys (always available in Main view)
            KeyCode::Char('1') if self.view == View::Main => {
                self.tab = Tab::Containers;
                return Ok(());
            }
            KeyCode::Char('2') if self.view == View::Main => {
                self.tab = Tab::Providers;
                return Ok(());
            }
            KeyCode::Char('3') if self.view == View::Main => {
                self.tab = Tab::Settings;
                return Ok(());
            }
            // Tab key cycles through tabs (in Main view)
            KeyCode::Tab if self.view == View::Main => {
                self.tab = match self.tab {
                    Tab::Containers => Tab::Providers,
                    Tab::Providers => Tab::Settings,
                    Tab::Settings => Tab::Containers,
                };
                return Ok(());
            }
            KeyCode::BackTab if self.view == View::Main => {
                self.tab = match self.tab {
                    Tab::Containers => Tab::Settings,
                    Tab::Providers => Tab::Containers,
                    Tab::Settings => Tab::Providers,
                };
                return Ok(());
            }
            _ => {}
        }

        // View/Tab-specific keys
        match self.view {
            View::Main => match self.tab {
                Tab::Containers => self.handle_containers_key(code, modifiers).await?,
                Tab::Providers => self.handle_providers_key(code, modifiers).await?,
                Tab::Settings => self.handle_settings_key(code, modifiers).await?,
            },
            View::ContainerDetail => self.handle_detail_key(code, modifiers).await?,
            View::ProviderDetail => self.handle_provider_detail_key(code, modifiers).await?,
            View::BuildOutput => self.handle_build_key(code, modifiers).await?,
            View::Logs => self.handle_logs_key(code, modifiers).await?,
            View::Help | View::Confirm => {} // Handled above
        }

        Ok(())
    }

    /// Handle Containers tab keys
    async fn handle_containers_key(
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
            KeyCode::Char('g') | KeyCode::Home => {
                self.selected = 0;
            }
            KeyCode::Char('G') | KeyCode::End => {
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
            KeyCode::Char('d') | KeyCode::Delete => {
                if !self.containers.is_empty() {
                    let container = &self.containers[self.selected];
                    self.confirm_action = Some(ConfirmAction::Delete(container.id.clone()));
                    self.view = View::Confirm;
                }
            }
            KeyCode::Char('r') | KeyCode::F(5) => {
                self.refresh_containers().await?;
                self.status_message = Some("Refreshed".to_string());
            }
            KeyCode::Char('R') => {
                if !self.containers.is_empty() {
                    let container = &self.containers[self.selected];
                    let old_provider = container.provider;
                    let new_provider = self.active_provider;
                    let provider_change = if old_provider != new_provider {
                        Some((old_provider, new_provider))
                    } else {
                        None
                    };

                    self.rebuild_no_cache = false;
                    self.confirm_action = Some(ConfirmAction::Rebuild {
                        id: container.id.clone(),
                        provider_change,
                    });
                    self.view = View::Confirm;
                }
            }

            _ => {}
        }
        Ok(())
    }

    /// Handle Providers tab keys
    async fn handle_providers_key(
        &mut self,
        code: KeyCode,
        _modifiers: KeyModifiers,
    ) -> AppResult<()> {
        match code {
            // Navigation
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.providers.is_empty() {
                    self.selected_provider = (self.selected_provider + 1) % self.providers.len();
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if !self.providers.is_empty() {
                    self.selected_provider = self.selected_provider
                        .checked_sub(1)
                        .unwrap_or(self.providers.len() - 1);
                }
            }

            // Open provider detail/configuration
            KeyCode::Enter => {
                if !self.providers.is_empty() {
                    // Reset provider detail state and enter detail view
                    self.provider_detail_state = ProviderDetailState::new();
                    self.view = View::ProviderDetail;
                }
            }

            // Set as active provider (quick toggle)
            KeyCode::Char(' ') | KeyCode::Char('a') => {
                if !self.providers.is_empty() {
                    let new_provider = self.providers[self.selected_provider].provider_type;
                    let provider_name = self.providers[self.selected_provider].name.clone();

                    // Update active status
                    for p in &mut self.providers {
                        p.is_active = p.provider_type == new_provider;
                    }
                    self.active_provider = new_provider;

                    // Update config
                    self.config.defaults.provider = match new_provider {
                        ProviderType::Docker => "docker".to_string(),
                        ProviderType::Podman => "podman".to_string(),
                    };

                    self.status_message = Some(format!(
                        "Active provider set to {}. Press 's' to save.",
                        provider_name
                    ));
                }
            }

            // Save changes
            KeyCode::Char('s') => {
                if let Err(e) = self.config.save() {
                    self.status_message = Some(format!("Failed to save: {}", e));
                } else {
                    self.status_message = Some("Provider settings saved".to_string());
                }
            }

            _ => {}
        }
        Ok(())
    }

    /// Handle Provider Detail view keys
    async fn handle_provider_detail_key(
        &mut self,
        code: KeyCode,
        _modifiers: KeyModifiers,
    ) -> AppResult<()> {
        let provider = &self.providers[self.selected_provider];
        let provider_type = provider.provider_type;

        if self.provider_detail_state.editing {
            // In edit mode
            match code {
                KeyCode::Enter => {
                    if let Some(new_value) = self.provider_detail_state.confirm_edit() {
                        // Update the socket path in config
                        match provider_type {
                            ProviderType::Docker => {
                                self.config.providers.docker.socket = new_value.clone();
                                self.providers[self.selected_provider].socket = new_value;
                            }
                            ProviderType::Podman => {
                                self.config.providers.podman.socket = new_value.clone();
                                self.providers[self.selected_provider].socket = new_value;
                            }
                        }
                        self.status_message = Some("Socket path updated. Press 's' to save.".to_string());
                    }
                }
                KeyCode::Esc => {
                    self.provider_detail_state.cancel_edit();
                }
                KeyCode::Backspace => {
                    self.provider_detail_state.delete_char();
                }
                KeyCode::Left => {
                    self.provider_detail_state.move_cursor_left();
                }
                KeyCode::Right => {
                    self.provider_detail_state.move_cursor_right();
                }
                KeyCode::Char(c) => {
                    self.provider_detail_state.insert_char(c);
                }
                _ => {}
            }
        } else {
            // Navigation mode
            match code {
                // Edit socket path
                KeyCode::Char('e') | KeyCode::Enter => {
                    let current_socket = self.providers[self.selected_provider].socket.clone();
                    self.provider_detail_state.start_edit(&current_socket);
                }

                // Test connection by checking if the socket exists
                KeyCode::Char('t') => {
                    self.status_message = Some("Testing connection...".to_string());
                    self.provider_detail_state.clear_connection_status();

                    let socket_path = &self.providers[self.selected_provider].socket;
                    let socket_exists = std::path::Path::new(socket_path).exists();

                    if socket_exists {
                        // Try to list containers as a connectivity test
                        match self.manager.list().await {
                            Ok(_) => {
                                self.provider_detail_state.set_connection_result(true, None);
                                self.providers[self.selected_provider].connected = true;
                                self.status_message = Some("Connection successful!".to_string());
                            }
                            Err(e) => {
                                self.provider_detail_state.set_connection_result(false, Some(e.to_string()));
                                self.providers[self.selected_provider].connected = false;
                                self.status_message = Some(format!("Connection failed: {}", e));
                            }
                        }
                    } else {
                        let msg = format!("Socket not found: {}", socket_path);
                        self.provider_detail_state.set_connection_result(false, Some(msg.clone()));
                        self.providers[self.selected_provider].connected = false;
                        self.status_message = Some(msg);
                    }
                }

                // Set as active provider
                KeyCode::Char('a') | KeyCode::Char(' ') => {
                    let new_provider = self.providers[self.selected_provider].provider_type;
                    let provider_name = self.providers[self.selected_provider].name.clone();

                    for p in &mut self.providers {
                        p.is_active = p.provider_type == new_provider;
                    }
                    self.active_provider = new_provider;

                    self.config.defaults.provider = match new_provider {
                        ProviderType::Docker => "docker".to_string(),
                        ProviderType::Podman => "podman".to_string(),
                    };

                    self.status_message = Some(format!("{} set as active. Press 's' to save.", provider_name));
                }

                // Save changes
                KeyCode::Char('s') => {
                    if let Err(e) = self.config.save() {
                        self.status_message = Some(format!("Failed to save: {}", e));
                    } else {
                        self.provider_detail_state.dirty = false;
                        self.status_message = Some("Provider settings saved".to_string());
                    }
                }

                _ => {}
            }
        }
        Ok(())
    }

    /// Handle Settings tab keys
    async fn handle_settings_key(
        &mut self,
        code: KeyCode,
        _modifiers: KeyModifiers,
    ) -> AppResult<()> {
        if self.settings_state.editing {
            // In edit mode
            match code {
                KeyCode::Enter => {
                    self.settings_state.confirm_edit();
                }
                KeyCode::Esc => {
                    self.settings_state.cancel_edit();
                }
                KeyCode::Backspace => {
                    self.settings_state.delete_char();
                }
                KeyCode::Left => {
                    self.settings_state.move_cursor_left();
                }
                KeyCode::Right => {
                    self.settings_state.move_cursor_right();
                }
                KeyCode::Char(c) => {
                    self.settings_state.insert_char(c);
                }
                _ => {}
            }
        } else {
            // Navigation mode
            match code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.settings_state.move_down();
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.settings_state.move_up();
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    self.settings_state.start_edit();
                }
                KeyCode::Char('s') => {
                    // Save settings
                    self.settings_state.apply_to_config(&mut self.config);
                    if let Err(e) = self.config.save() {
                        self.status_message = Some(format!("Failed to save: {}", e));
                    } else {
                        self.status_message = Some("Settings saved".to_string());
                        self.settings_state.dirty = false;
                    }
                }
                KeyCode::Char('r') => {
                    // Reset to saved values
                    self.settings_state.reset_from_config(&self.config);
                    self.status_message = Some("Settings reset to saved values".to_string());
                }
                _ => {}
            }
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
            KeyCode::Char('R') => {
                if !self.containers.is_empty() {
                    let container = &self.containers[self.selected];
                    let old_provider = container.provider;
                    let new_provider = self.active_provider;
                    let provider_change = if old_provider != new_provider {
                        Some((old_provider, new_provider))
                    } else {
                        None
                    };

                    self.rebuild_no_cache = false;
                    self.confirm_action = Some(ConfirmAction::Rebuild {
                        id: container.id.clone(),
                        provider_change,
                    });
                    self.view = View::Confirm;
                }
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
                if self.build_output_scroll < self.build_output.len().saturating_sub(1) {
                    self.build_output_scroll += 1;
                    self.build_auto_scroll = false; // User took control
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.build_output_scroll > 0 {
                    self.build_output_scroll -= 1;
                    self.build_auto_scroll = false;
                }
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.build_output_scroll = self.build_output.len().saturating_sub(1);
                self.build_auto_scroll = true; // Re-enable auto-scroll
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.build_output_scroll = 0;
                self.build_auto_scroll = false;
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                // Only allow closing after build completes
                if self.build_complete {
                    self.view = View::Main;
                    self.build_output.clear();
                    self.build_output_scroll = 0;
                    self.build_complete = false;
                    self.build_auto_scroll = true;
                }
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
        let page_size = 20;

        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                if self.logs_scroll < self.logs.len().saturating_sub(1) {
                    self.logs_scroll += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.logs_scroll = self.logs_scroll.saturating_sub(1);
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.logs_scroll = 0;
            }
            KeyCode::Char('G') | KeyCode::End => {
                self.logs_scroll = self.logs.len().saturating_sub(1);
            }
            KeyCode::Char('d') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.logs_scroll = (self.logs_scroll + page_size / 2)
                    .min(self.logs.len().saturating_sub(1));
            }
            KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => {
                self.logs_scroll = self.logs_scroll.saturating_sub(page_size / 2);
            }
            KeyCode::PageDown => {
                self.logs_scroll = (self.logs_scroll + page_size)
                    .min(self.logs.len().saturating_sub(1));
            }
            KeyCode::PageUp => {
                self.logs_scroll = self.logs_scroll.saturating_sub(page_size);
            }
            KeyCode::Char('r') | KeyCode::F(5) => {
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

        if container.container_id.is_none() {
            self.status_message = Some("Container has not been created yet".to_string());
            return Ok(());
        }

        self.status_message = Some(format!("Loading logs for {}...", container.name));
        self.loading = true;

        match self.manager.logs(&container.id, Some(1000)).await {
            Ok(lines) => {
                self.logs = lines;
                self.logs_scroll = self.logs.len().saturating_sub(1);
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
            ConfirmAction::Rebuild { id, .. } => {
                self.loading = true;
                self.view = View::BuildOutput;
                self.build_output.clear();
                self.build_output_scroll = 0;
                self.build_auto_scroll = true;
                self.build_complete = false;

                // Create channel for progress updates
                let (tx, rx) = mpsc::unbounded_channel();
                self.build_progress_rx = Some(rx);

                // Clone values for the background task
                let manager = Arc::clone(&self.manager);
                let no_cache = self.rebuild_no_cache;
                self.rebuild_no_cache = false;

                // Spawn background task for rebuild
                tokio::spawn(async move {
                    let _ = tx.send("Starting rebuild...".to_string());
                    match manager.rebuild_with_progress(&id, no_cache, tx.clone()).await {
                        Ok(()) => {
                            // Success message is sent by rebuild_with_progress
                        }
                        Err(e) => {
                            let _ = tx.send(format!("Error: Rebuild failed: {}", e));
                        }
                    }
                });
            }
        }
        Ok(())
    }

    /// Get the currently selected container
    pub fn selected_container(&self) -> Option<&ContainerState> {
        self.containers.get(self.selected)
    }
}
