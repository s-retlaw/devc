//! Main TUI application state and logic

use crate::clipboard::copy_to_clipboard;
use crate::event::{Event, EventHandler};
use crate::ports::{spawn_port_detector, DetectedPort, PortDetectionUpdate};
use crate::settings::{ProviderDetailState, SettingsState};
use crate::tunnel::{open_in_browser, spawn_forwarder, PortForwarder};
use crate::ui;
use crossterm::event::{KeyCode, KeyModifiers};
use devc_config::GlobalConfig;
use devc_core::{ContainerManager, ContainerState, DevcContainerStatus};
use devc_provider::{create_provider, detect_available_providers, ContainerProvider, DiscoveredContainer, ProviderType};
use ratatui::prelude::*;
use ratatui::widgets::TableState;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{mpsc, RwLock};

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
    /// Port forwarding view
    Ports,
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
    /// Set a provider as the default and save to config
    SetDefaultProvider(ProviderType),
    /// Adopt a discovered container into devc management
    Adopt {
        container_id: String,
        container_name: String,
        workspace_path: Option<String>,
    },
}

/// Dialog focus state for keyboard navigation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DialogFocus {
    /// Checkbox (for dialogs that have one)
    Checkbox,
    /// Confirm/Yes button
    Confirm,
    /// Cancel/No button
    #[default]
    Cancel,
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
    /// Container manager (wrapped in Arc<RwLock> for reconnection support)
    pub manager: Arc<RwLock<ContainerManager>>,
    /// Global configuration
    pub config: GlobalConfig,
    /// Current tab
    pub tab: Tab,
    /// Current view within the tab
    pub view: View,
    /// Active provider type (for new containers), None if disconnected
    pub active_provider: Option<ProviderType>,
    /// Provider statuses
    pub providers: Vec<ProviderStatus>,
    /// Selected provider index (in Providers tab)
    pub selected_provider: usize,
    /// Connection error message (if disconnected)
    pub connection_error: Option<String>,
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
    /// Dialog focus state for keyboard navigation
    pub dialog_focus: DialogFocus,
    /// Settings state (for global settings)
    pub settings_state: SettingsState,
    /// Provider detail state (for provider-specific settings)
    pub provider_detail_state: ProviderDetailState,
    /// Whether we're in discover mode (showing all devcontainers, not just managed)
    pub discover_mode: bool,
    /// Discovered containers (when in discover mode)
    pub discovered_containers: Vec<DiscoveredContainer>,
    /// Selected discovered container index
    pub selected_discovered: usize,
    /// Table state for containers view (tracks selection and scroll)
    pub containers_table_state: TableState,
    /// Table state for discovered containers view
    pub discovered_table_state: TableState,
    /// Table state for providers view
    pub providers_table_state: TableState,

    // Port forwarding state
    /// Container currently being viewed for port forwarding (container_id from provider)
    pub ports_container_id: Option<String>,
    /// Provider container ID for the ports view
    pub ports_provider_container_id: Option<String>,
    /// Detected ports in current container
    pub detected_ports: Vec<DetectedPort>,
    /// Selected port index
    pub selected_port: usize,
    /// Table state for port list
    pub ports_table_state: TableState,
    /// Receiver for port detection updates
    pub port_detect_rx: Option<mpsc::UnboundedReceiver<PortDetectionUpdate>>,

    // Port forwarder management (persists across views)
    /// Active port forwarders: (container_id, port) -> PortForwarder
    pub active_forwarders: HashMap<(String, u16), PortForwarder>,
}

impl App {
    /// Create an App for testing without requiring a ContainerManager
    ///
    /// This is useful for unit tests and snapshot tests.
    pub fn new_for_testing() -> Self {
        use devc_provider::ProviderType;

        let config = GlobalConfig::default();
        let manager = ContainerManager::disconnected(config.clone(), "Test mode".to_string())
            .expect("Failed to create test manager");

        Self {
            manager: Arc::new(RwLock::new(manager)),
            config,
            tab: Tab::Containers,
            view: View::Main,
            active_provider: Some(ProviderType::Docker),
            providers: vec![
                ProviderStatus {
                    provider_type: ProviderType::Docker,
                    name: "Docker".to_string(),
                    socket: "/var/run/docker.sock".to_string(),
                    connected: true,
                    is_active: true,
                },
                ProviderStatus {
                    provider_type: ProviderType::Podman,
                    name: "Podman".to_string(),
                    socket: "/run/user/1000/podman/podman.sock".to_string(),
                    connected: false,
                    is_active: false,
                },
            ],
            selected_provider: 0,
            connection_error: None,
            containers: Vec::new(),
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
            dialog_focus: DialogFocus::default(),
            settings_state: SettingsState::new(&GlobalConfig::default()),
            provider_detail_state: ProviderDetailState::new(),
            discover_mode: false,
            discovered_containers: Vec::new(),
            selected_discovered: 0,
            containers_table_state: TableState::default().with_selected(0),
            discovered_table_state: TableState::default().with_selected(0),
            providers_table_state: TableState::default().with_selected(0),
            // Port forwarding
            ports_container_id: None,
            ports_provider_container_id: None,
            detected_ports: Vec::new(),
            selected_port: 0,
            ports_table_state: TableState::default().with_selected(0),
            port_detect_rx: None,
            active_forwarders: HashMap::new(),
        }
    }

    /// Create a test container state for testing
    ///
    /// This is useful for unit tests and snapshot tests.
    /// Uses a fixed timestamp to ensure deterministic test output.
    pub fn create_test_container(name: &str, status: DevcContainerStatus) -> ContainerState {
        use chrono::{TimeZone, Utc};
        use devc_provider::ProviderType;
        use std::collections::HashMap;
        use std::path::PathBuf;

        // Use fixed timestamp for deterministic snapshots
        let fixed_time = Utc.with_ymd_and_hms(2024, 1, 15, 12, 0, 0).unwrap();

        ContainerState {
            id: format!("test-{}", name),
            name: name.to_string(),
            provider: ProviderType::Docker,
            config_path: PathBuf::from("/tmp/test/.devcontainer/devcontainer.json"),
            image_id: Some("sha256:abc123".to_string()),
            container_id: Some(format!("container-{}", name)),
            status,
            created_at: fixed_time,
            last_used: fixed_time,
            workspace_path: PathBuf::from("/tmp/test"),
            metadata: HashMap::new(),
        }
    }

    /// Create a new application
    pub async fn new(manager: ContainerManager) -> AppResult<Self> {
        let containers = manager.list().await?;
        let config = GlobalConfig::load().unwrap_or_default();
        let active_provider = manager.provider_type();
        let connection_error = manager.connection_error().map(|s| s.to_string());
        let settings_state = SettingsState::new(&config);

        // Test all providers at startup to show accurate connection status
        let available_providers = detect_available_providers(&config).await;
        let docker_connected = available_providers.iter()
            .find(|(t, _)| *t == ProviderType::Docker)
            .map(|(_, connected)| *connected)
            .unwrap_or(false);
        let podman_connected = available_providers.iter()
            .find(|(t, _)| *t == ProviderType::Podman)
            .map(|(_, connected)| *connected)
            .unwrap_or(false);

        // Build provider status list with accurate connection status
        // Put Docker first (more common on Windows/Mac)
        let providers = vec![
            ProviderStatus {
                provider_type: ProviderType::Docker,
                name: "Docker".to_string(),
                socket: config.providers.docker.socket.clone(),
                connected: docker_connected,
                is_active: active_provider == Some(ProviderType::Docker),
            },
            ProviderStatus {
                provider_type: ProviderType::Podman,
                name: "Podman".to_string(),
                socket: config.providers.podman.socket.clone(),
                connected: podman_connected,
                is_active: active_provider == Some(ProviderType::Podman),
            },
        ];

        Ok(Self {
            manager: Arc::new(RwLock::new(manager)),
            config,
            tab: Tab::Containers,
            view: View::Main,
            active_provider,
            providers,
            selected_provider: if active_provider == Some(ProviderType::Podman) { 1 } else { 0 },
            connection_error,
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
            dialog_focus: DialogFocus::default(),
            settings_state,
            provider_detail_state: ProviderDetailState::new(),
            discover_mode: false,
            discovered_containers: Vec::new(),
            selected_discovered: 0,
            containers_table_state: TableState::default().with_selected(0),
            discovered_table_state: TableState::default().with_selected(0),
            providers_table_state: TableState::default().with_selected(0),
            // Port forwarding
            ports_container_id: None,
            ports_provider_container_id: None,
            detected_ports: Vec::new(),
            selected_port: 0,
            ports_table_state: TableState::default().with_selected(0),
            port_detect_rx: None,
            active_forwarders: HashMap::new(),
        })
    }

    /// Check if connected to a container provider
    pub fn is_connected(&self) -> bool {
        self.active_provider.is_some()
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
                // Port detection updates
                ports = Self::recv_port_update(&mut self.port_detect_rx) => {
                    if let Some(update) = ports {
                        self.handle_port_update(update);
                    }
                }
            }
        }

        // Cleanup: stop all forwarders on exit
        for (_, forwarder) in self.active_forwarders.drain() {
            forwarder.stop().await;
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

    /// Helper to receive port detection updates
    async fn recv_port_update(
        rx: &mut Option<mpsc::UnboundedReceiver<PortDetectionUpdate>>,
    ) -> Option<PortDetectionUpdate> {
        match rx {
            Some(ref mut receiver) => receiver.recv().await,
            None => std::future::pending().await,
        }
    }

    /// Handle port detection update
    fn handle_port_update(&mut self, update: PortDetectionUpdate) {
        // Update is_forwarded based on active tunnels
        let forwarded_ports: HashSet<u16> = if let Some(ref container_id) = self.ports_provider_container_id {
            self.active_forwarders
                .keys()
                .filter(|(cid, _)| cid == container_id)
                .map(|(_, port)| *port)
                .collect()
        } else {
            HashSet::new()
        };

        self.detected_ports = update
            .ports
            .into_iter()
            .map(|mut p| {
                p.is_forwarded = forwarded_ports.contains(&p.port);
                p
            })
            .collect();

        // Update table state if needed
        if !self.detected_ports.is_empty() && self.selected_port >= self.detected_ports.len() {
            self.selected_port = self.detected_ports.len() - 1;
        }
        if !self.detected_ports.is_empty() {
            self.ports_table_state.select(Some(self.selected_port));
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
            let has_checkbox = matches!(self.confirm_action, Some(ConfirmAction::Rebuild { .. }));

            match code {
                // Tab moves to next focusable element
                KeyCode::Tab => {
                    self.dialog_focus = match self.dialog_focus {
                        DialogFocus::Checkbox => DialogFocus::Confirm,
                        DialogFocus::Confirm => DialogFocus::Cancel,
                        DialogFocus::Cancel => {
                            if has_checkbox {
                                DialogFocus::Checkbox
                            } else {
                                DialogFocus::Confirm
                            }
                        }
                    };
                }
                // Shift+Tab moves to previous focusable element
                KeyCode::BackTab => {
                    self.dialog_focus = match self.dialog_focus {
                        DialogFocus::Checkbox => DialogFocus::Cancel,
                        DialogFocus::Confirm => {
                            if has_checkbox {
                                DialogFocus::Checkbox
                            } else {
                                DialogFocus::Cancel
                            }
                        }
                        DialogFocus::Cancel => DialogFocus::Confirm,
                    };
                }
                // Enter activates the focused element
                KeyCode::Enter => {
                    match self.dialog_focus {
                        DialogFocus::Checkbox => {
                            // Toggle checkbox
                            self.rebuild_no_cache = !self.rebuild_no_cache;
                        }
                        DialogFocus::Confirm => {
                            // Execute the action
                            if let Some(action) = self.confirm_action.take() {
                                self.execute_confirm_action(action).await?;
                            }
                            // Only return to Main if the action didn't change the view
                            if self.view == View::Confirm {
                                self.view = View::Main;
                            }
                        }
                        DialogFocus::Cancel => {
                            // Cancel
                            self.confirm_action = None;
                            self.rebuild_no_cache = false;
                            self.dialog_focus = DialogFocus::default();
                            self.view = View::Main;
                        }
                    }
                }
                // Space toggles checkbox if focused, otherwise acts like Enter
                KeyCode::Char(' ') => {
                    if self.dialog_focus == DialogFocus::Checkbox {
                        self.rebuild_no_cache = !self.rebuild_no_cache;
                    } else {
                        // Treat space like Enter for buttons
                        match self.dialog_focus {
                            DialogFocus::Confirm => {
                                if let Some(action) = self.confirm_action.take() {
                                    self.execute_confirm_action(action).await?;
                                }
                                if self.view == View::Confirm {
                                    self.view = View::Main;
                                }
                            }
                            DialogFocus::Cancel => {
                                self.confirm_action = None;
                                self.rebuild_no_cache = false;
                                self.dialog_focus = DialogFocus::default();
                                self.view = View::Main;
                            }
                            _ => {}
                        }
                    }
                }
                // Shortcut keys still work
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(action) = self.confirm_action.take() {
                        self.execute_confirm_action(action).await?;
                    }
                    if self.view == View::Confirm {
                        self.view = View::Main;
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirm_action = None;
                    self.rebuild_no_cache = false;
                    self.dialog_focus = DialogFocus::default();
                    self.view = View::Main;
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

        // Check discover mode FIRST - Esc/q should exit discover mode, not quit app
        if self.view == View::Main && self.tab == Tab::Containers && self.discover_mode {
            if code == KeyCode::Esc || code == KeyCode::Char('q') {
                self.discover_mode = false;
                self.status_message = Some("Showing managed containers".to_string());
                return Ok(());
            }
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
            View::Ports => self.handle_ports_key(code, modifiers).await?,
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
        // Toggle discover mode with 'D'
        if code == KeyCode::Char('D') {
            if self.discover_mode {
                // Exit discover mode
                self.discover_mode = false;
                self.status_message = Some("Showing managed containers".to_string());
            } else {
                // Enter discover mode
                self.discover_mode = true;
                self.status_message = Some("Discovering containers...".to_string());
                self.refresh_discovered().await?;
                self.status_message = Some("Discover mode: showing all devcontainers".to_string());
            }
            return Ok(());
        }

        if self.discover_mode {
            // Discover mode key handling
            match code {
                // Navigation
                KeyCode::Char('j') | KeyCode::Down => {
                    if !self.discovered_containers.is_empty() {
                        self.selected_discovered = (self.selected_discovered + 1) % self.discovered_containers.len();
                        self.discovered_table_state.select(Some(self.selected_discovered));
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if !self.discovered_containers.is_empty() {
                        self.selected_discovered = self.selected_discovered
                            .checked_sub(1)
                            .unwrap_or(self.discovered_containers.len() - 1);
                        self.discovered_table_state.select(Some(self.selected_discovered));
                    }
                }
                KeyCode::Char('g') | KeyCode::Home => {
                    self.selected_discovered = 0;
                    self.discovered_table_state.select(Some(0));
                }
                KeyCode::Char('G') | KeyCode::End => {
                    if !self.discovered_containers.is_empty() {
                        self.selected_discovered = self.discovered_containers.len() - 1;
                        self.discovered_table_state.select(Some(self.selected_discovered));
                    }
                }
                // Adopt selected container
                KeyCode::Char('a') => {
                    if !self.discovered_containers.is_empty() {
                        let container = &self.discovered_containers[self.selected_discovered];
                        if !container.managed {
                            self.dialog_focus = DialogFocus::Cancel;
                            self.confirm_action = Some(ConfirmAction::Adopt {
                                container_id: container.id.0.clone(),
                                container_name: container.name.clone(),
                                workspace_path: container.workspace_path.clone(),
                            });
                            self.view = View::Confirm;
                        } else {
                            self.status_message = Some("Container is already managed by devc".to_string());
                        }
                    }
                }
                // Refresh discovered containers
                KeyCode::Char('r') | KeyCode::F(5) => {
                    self.refresh_discovered().await?;
                    self.status_message = Some("Refreshed discovered containers".to_string());
                }
                _ => {}
            }
        } else {
            // Normal (managed) mode key handling
            match code {
                // Navigation
                KeyCode::Char('j') | KeyCode::Down => {
                    if !self.containers.is_empty() {
                        self.selected = (self.selected + 1) % self.containers.len();
                        self.containers_table_state.select(Some(self.selected));
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if !self.containers.is_empty() {
                        self.selected = self.selected.checked_sub(1).unwrap_or(self.containers.len() - 1);
                        self.containers_table_state.select(Some(self.selected));
                    }
                }
                KeyCode::Char('g') | KeyCode::Home => {
                    self.selected = 0;
                    self.containers_table_state.select(Some(0));
                }
                KeyCode::Char('G') | KeyCode::End => {
                    if !self.containers.is_empty() {
                        self.selected = self.containers.len() - 1;
                        self.containers_table_state.select(Some(self.selected));
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
                        self.dialog_focus = DialogFocus::Cancel;
                        self.view = View::Confirm;
                    }
                }
                KeyCode::Char('r') | KeyCode::F(5) => {
                    self.refresh_containers().await?;
                    self.status_message = Some("Refreshed".to_string());
                }
                KeyCode::Char('R') => {
                    if !self.containers.is_empty() && self.is_connected() {
                        let container = &self.containers[self.selected];
                        let old_provider = container.provider;
                        let new_provider = self.active_provider.unwrap(); // Safe: checked is_connected
                        let provider_change = if old_provider != new_provider {
                            Some((old_provider, new_provider))
                        } else {
                            None
                        };

                        self.rebuild_no_cache = false;
                        self.dialog_focus = DialogFocus::Cancel;
                        self.confirm_action = Some(ConfirmAction::Rebuild {
                            id: container.id.clone(),
                            provider_change,
                        });
                        self.view = View::Confirm;
                    } else if !self.is_connected() {
                        self.status_message = Some("Not connected to provider".to_string());
                    }
                }
                KeyCode::Char('p') => {
                    // Enter port forwarding view for selected container
                    if !self.containers.is_empty() {
                        let container = self.containers[self.selected].clone();
                        self.enter_ports_view(&container).await?;
                    }
                }

                _ => {}
            }
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
                    self.providers_table_state.select(Some(self.selected_provider));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if !self.providers.is_empty() {
                    self.selected_provider = self.selected_provider
                        .checked_sub(1)
                        .unwrap_or(self.providers.len() - 1);
                    self.providers_table_state.select(Some(self.selected_provider));
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

            // Set as active provider - show confirmation dialog
            KeyCode::Char(' ') | KeyCode::Char('a') => {
                if !self.providers.is_empty() {
                    let new_provider = self.providers[self.selected_provider].provider_type;
                    // Only show confirmation if it's a different provider
                    if self.active_provider != Some(new_provider) {
                        self.dialog_focus = DialogFocus::Cancel;
                        self.confirm_action = Some(ConfirmAction::SetDefaultProvider(new_provider));
                        self.view = View::Confirm;
                    }
                }
            }

            // Save changes (for socket path edits)
            KeyCode::Char('s') => {
                if let Err(e) = self.config.save() {
                    self.status_message = Some(format!("Failed to save: {}", e));
                } else {
                    self.status_message = Some("Provider settings saved".to_string());
                }
            }

            // Retry connection
            KeyCode::Char('c') => {
                if !self.is_connected() {
                    self.retry_connection().await?;
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
                        match self.manager.read().await.list().await {
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

                // Set as active provider - show confirmation dialog
                KeyCode::Char('a') | KeyCode::Char(' ') => {
                    let new_provider = self.providers[self.selected_provider].provider_type;
                    // Only show confirmation if it's a different provider
                    if self.active_provider != Some(new_provider) {
                        self.dialog_focus = DialogFocus::Cancel;
                        self.confirm_action = Some(ConfirmAction::SetDefaultProvider(new_provider));
                        self.view = View::Confirm;
                    }
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
                if !self.containers.is_empty() && self.is_connected() {
                    let container = &self.containers[self.selected];
                    let old_provider = container.provider;
                    let new_provider = self.active_provider.unwrap(); // Safe: checked is_connected
                    let provider_change = if old_provider != new_provider {
                        Some((old_provider, new_provider))
                    } else {
                        None
                    };

                    self.rebuild_no_cache = false;
                    self.dialog_focus = DialogFocus::Cancel;
                    self.confirm_action = Some(ConfirmAction::Rebuild {
                        id: container.id.clone(),
                        provider_change,
                    });
                    self.view = View::Confirm;
                } else if !self.is_connected() {
                    self.status_message = Some("Not connected to provider".to_string());
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
            KeyCode::Char('c') => {
                // Copy all log lines to clipboard
                let content = self.build_output.join("\n");
                if let Err(e) = copy_to_clipboard(&content) {
                    self.status_message = Some(format!("Failed to copy: {}", e));
                } else {
                    self.status_message = Some(format!("Copied {} lines to clipboard", self.build_output.len()));
                }
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

    /// Handle Port Forwarding view keys
    async fn handle_ports_key(
        &mut self,
        code: KeyCode,
        _modifiers: KeyModifiers,
    ) -> AppResult<()> {
        match code {
            // Navigation
            KeyCode::Char('j') | KeyCode::Down => {
                if !self.detected_ports.is_empty() {
                    self.selected_port = (self.selected_port + 1) % self.detected_ports.len();
                    self.ports_table_state.select(Some(self.selected_port));
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if !self.detected_ports.is_empty() {
                    self.selected_port = self.selected_port
                        .checked_sub(1)
                        .unwrap_or(self.detected_ports.len() - 1);
                    self.ports_table_state.select(Some(self.selected_port));
                }
            }
            KeyCode::Char('g') | KeyCode::Home => {
                if !self.detected_ports.is_empty() {
                    self.selected_port = 0;
                    self.ports_table_state.select(Some(0));
                }
            }
            KeyCode::Char('G') | KeyCode::End => {
                if !self.detected_ports.is_empty() {
                    self.selected_port = self.detected_ports.len() - 1;
                    self.ports_table_state.select(Some(self.selected_port));
                }
            }

            // Forward selected port
            KeyCode::Char('f') => {
                if let Some(port) = self.detected_ports.get(self.selected_port) {
                    if !port.is_forwarded {
                        self.forward_port(port.port).await?;
                    }
                }
            }

            // Stop forwarding
            KeyCode::Char('s') => {
                if let Some(port) = self.detected_ports.get(self.selected_port) {
                    if port.is_forwarded {
                        self.stop_forward(port.port).await;
                    }
                }
            }

            // Open in browser
            KeyCode::Char('o') => {
                if let Some(port) = self.detected_ports.get(self.selected_port) {
                    if port.is_forwarded {
                        if let Err(e) = open_in_browser(port.port) {
                            self.status_message = Some(format!("Failed to open browser: {}", e));
                        }
                    } else {
                        self.status_message = Some("Port must be forwarded first".to_string());
                    }
                }
            }

            // Forward all
            KeyCode::Char('a') => {
                let ports_to_forward: Vec<u16> = self
                    .detected_ports
                    .iter()
                    .filter(|p| !p.is_forwarded)
                    .map(|p| p.port)
                    .collect();
                for port in ports_to_forward {
                    self.forward_port(port).await?;
                }
            }

            // Stop all (none)
            KeyCode::Char('n') => {
                self.stop_all_forwards_for_container().await;
            }

            // Back to containers (q and Esc handled globally, but we handle here too for safety)
            KeyCode::Char('q') | KeyCode::Esc => {
                self.exit_ports_view();
            }

            _ => {}
        }
        Ok(())
    }

    /// Enter port forwarding view for a container
    async fn enter_ports_view(&mut self, container: &ContainerState) -> AppResult<()> {
        // Check container is running
        if container.status != DevcContainerStatus::Running {
            self.status_message = Some("Container must be running to forward ports".to_string());
            return Ok(());
        }

        // Get provider container ID
        let provider_container_id = match &container.container_id {
            Some(id) => id.clone(),
            None => {
                self.status_message = Some("Container has not been created yet".to_string());
                return Ok(());
            }
        };

        self.view = View::Ports;
        self.ports_container_id = Some(container.id.clone());
        self.ports_provider_container_id = Some(provider_container_id.clone());
        self.detected_ports.clear();
        self.selected_port = 0;
        self.ports_table_state.select(Some(0));

        // Get forwarded ports for this container
        let forwarded_ports: HashSet<u16> = self
            .active_forwarders
            .keys()
            .filter(|(cid, _)| cid == &provider_container_id)
            .map(|(_, port)| *port)
            .collect();

        // Start port detection polling - create a new provider instance for the background task
        let provider_type = self.active_provider.unwrap_or(ProviderType::Docker);
        let provider_result = match provider_type {
            ProviderType::Docker => devc_provider::CliProvider::new_docker().await,
            ProviderType::Podman => devc_provider::CliProvider::new_podman().await,
        };

        match provider_result {
            Ok(provider) => {
                let provider_arc: Arc<dyn ContainerProvider + Send + Sync> = Arc::new(provider);
                let container_id = devc_provider::ContainerId::new(&provider_container_id);
                let rx = spawn_port_detector(provider_arc, container_id, provider_type, forwarded_ports);
                self.port_detect_rx = Some(rx);
                self.status_message = Some("Detecting ports...".to_string());
            }
            Err(e) => {
                self.status_message = Some(format!("Failed to start port detection: {}", e));
            }
        }

        Ok(())
    }

    /// Exit port forwarding view
    fn exit_ports_view(&mut self) {
        self.view = View::Main;
        self.ports_container_id = None;
        self.ports_provider_container_id = None;
        self.port_detect_rx = None; // Stops the polling task
        self.detected_ports.clear();
        // Note: tunnels are NOT killed here - they persist
    }

    /// Forward a port from the current container
    async fn forward_port(&mut self, port: u16) -> AppResult<()> {
        let container_id = match &self.ports_provider_container_id {
            Some(id) => id.clone(),
            None => return Ok(()),
        };

        let provider_type = self.active_provider.unwrap_or(ProviderType::Docker);

        // Spawn forwarder (uses socat via exec, no SSH needed)
        match spawn_forwarder(provider_type, &container_id, port, port).await {
            Ok(forwarder) => {
                self.active_forwarders.insert((container_id.clone(), port), forwarder);
                // Update detected_ports to reflect forwarded state
                if let Some(p) = self.detected_ports.iter_mut().find(|p| p.port == port) {
                    p.is_forwarded = true;
                }
                self.status_message = Some(format!("Forwarding port {} -> localhost:{}", port, port));
            }
            Err(e) => {
                self.status_message = Some(format!("Failed to forward port {}: {}", port, e));
            }
        }
        Ok(())
    }

    /// Stop forwarding a port
    async fn stop_forward(&mut self, port: u16) {
        let container_id = match &self.ports_provider_container_id {
            Some(id) => id.clone(),
            None => return,
        };

        let key = (container_id, port);
        if let Some(forwarder) = self.active_forwarders.remove(&key) {
            forwarder.stop().await;
            // Update detected_ports to reflect not forwarded state
            if let Some(p) = self.detected_ports.iter_mut().find(|p| p.port == port) {
                p.is_forwarded = false;
            }
            self.status_message = Some(format!("Stopped forwarding port {}", port));
        }
    }

    /// Stop all port forwards for the current container
    async fn stop_all_forwards_for_container(&mut self) {
        let container_id = match &self.ports_provider_container_id {
            Some(id) => id.clone(),
            None => return,
        };

        let keys_to_remove: Vec<(String, u16)> = self
            .active_forwarders
            .keys()
            .filter(|(cid, _)| cid == &container_id)
            .cloned()
            .collect();

        for key in keys_to_remove {
            if let Some(forwarder) = self.active_forwarders.remove(&key) {
                forwarder.stop().await;
            }
        }

        // Update all detected_ports to not forwarded
        for p in &mut self.detected_ports {
            p.is_forwarded = false;
        }
        self.status_message = Some("Stopped all port forwards".to_string());
    }

    /// Refresh container list
    async fn refresh_containers(&mut self) -> AppResult<()> {
        self.containers = self.manager.read().await.list().await?;

        // Sync status for all containers
        for container in &self.containers {
            let _ = self.manager.read().await.sync_status(&container.id).await;
        }

        // Re-fetch after sync
        self.containers = self.manager.read().await.list().await?;

        // Ensure selected index is valid
        if !self.containers.is_empty() && self.selected >= self.containers.len() {
            self.selected = self.containers.len() - 1;
        }
        // Sync table state
        if !self.containers.is_empty() {
            self.containers_table_state.select(Some(self.selected));
        }

        Ok(())
    }

    /// Refresh discovered containers list
    async fn refresh_discovered(&mut self) -> AppResult<()> {
        self.discovered_containers = self.manager.read().await.discover().await
            .unwrap_or_default();

        // Ensure selected index is valid
        if !self.discovered_containers.is_empty() && self.selected_discovered >= self.discovered_containers.len() {
            self.selected_discovered = self.discovered_containers.len() - 1;
        }
        // Sync table state
        if !self.discovered_containers.is_empty() {
            self.discovered_table_state.select(Some(self.selected_discovered));
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

        match self.manager.read().await.build(&container.id).await {
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
                match self.manager.read().await.stop(&container.id).await {
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
                match self.manager.read().await.start(&container.id).await {
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

        match self.manager.read().await.up(&container.id).await {
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

        match self.manager.read().await.logs(&container.id, Some(1000)).await {
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
                match self.manager.read().await.remove(&id, true).await {
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
                match self.manager.read().await.stop(&id).await {
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
                    match manager.read().await.rebuild_with_progress(&id, no_cache, tx.clone()).await {
                        Ok(()) => {
                            // Success message is sent by rebuild_with_progress
                        }
                        Err(e) => {
                            let _ = tx.send(format!("Error: Rebuild failed: {}", e));
                        }
                    }
                });
            }
            ConfirmAction::SetDefaultProvider(new_provider) => {
                let provider_name = match new_provider {
                    ProviderType::Docker => "Docker",
                    ProviderType::Podman => "Podman",
                };

                // Update active status in providers list
                for p in &mut self.providers {
                    p.is_active = p.provider_type == new_provider;
                }
                self.active_provider = Some(new_provider);

                // Update config
                self.config.defaults.provider = match new_provider {
                    ProviderType::Docker => "docker".to_string(),
                    ProviderType::Podman => "podman".to_string(),
                };

                // Save immediately
                if let Err(e) = self.config.save() {
                    self.status_message = Some(format!("Failed to save: {}", e));
                } else {
                    self.status_message = Some(format!("{} set as default provider", provider_name));

                    // Try to reconnect with the new provider
                    self.retry_connection().await?;
                }
            }
            ConfirmAction::Adopt { container_id, container_name, workspace_path } => {
                self.loading = true;
                self.status_message = Some(format!("Adopting '{}'...", container_name));

                // Use a block to ensure the read guard is dropped before refresh_containers
                let adopt_result = {
                    let manager = self.manager.read().await;
                    manager.adopt(&container_id, workspace_path.as_deref()).await
                };

                match adopt_result {
                    Ok(state) => {
                        self.status_message = Some(format!("Adopted '{}'", state.name));
                        // Switch back to managed view and refresh
                        self.discover_mode = false;
                        self.refresh_containers().await?;
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Failed to adopt: {}", e));
                    }
                }
                self.loading = false;
            }
        }
        Ok(())
    }

    /// Get the currently selected container
    pub fn selected_container(&self) -> Option<&ContainerState> {
        self.containers.get(self.selected)
    }

    /// Retry connection to the selected provider
    async fn retry_connection(&mut self) -> AppResult<()> {
        self.status_message = Some("Attempting to connect...".to_string());

        // Get the provider type from the selected provider
        let provider_type = self.providers[self.selected_provider].provider_type;

        // Try to create the provider
        match create_provider(provider_type, &self.config).await {
            Ok(provider) => {
                // Successfully connected - update the manager
                {
                    let mut manager = self.manager.write().await;
                    manager.connect(provider);
                }

                // Update app state
                self.active_provider = Some(provider_type);
                self.connection_error = None;

                // Update provider status
                for p in &mut self.providers {
                    p.connected = p.provider_type == provider_type;
                    p.is_active = p.provider_type == provider_type;
                }

                // Refresh container list
                self.containers = self.manager.read().await.list().await?;

                self.status_message = Some(format!("Connected to {}", provider_type));
            }
            Err(e) => {
                self.connection_error = Some(e.to_string());
                self.status_message = Some(format!("Connection failed: {}", e));
            }
        }

        Ok(())
    }
}
