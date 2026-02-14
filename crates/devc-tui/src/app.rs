//! Main TUI application state and logic

use crate::clipboard::copy_to_clipboard;
use crate::event::{Event, EventHandler};
use crate::ports::{spawn_port_detector, DetectedPort, PortDetectionUpdate};
use crate::settings::{ProviderDetailState, SettingsState};
#[cfg(unix)]
use crate::shell::PtyShell;
use crate::shell::{ShellConfig, ShellExitReason};
use crate::tunnel::{check_socat_installed, install_socat, open_in_browser, spawn_forwarder, InstallResult, PortForwarder};
use crate::{resume_tui, suspend_tui, ui};
use crossterm::event::{KeyCode, KeyModifiers};
use devc_config::GlobalConfig;
use devc_core::{Container, ContainerManager, ContainerState, DevcContainerStatus};
use devc_provider::{create_provider, detect_available_providers, ContainerProvider, DevcontainerSource, DiscoveredContainer, ProviderType};
use ratatui::prelude::*;
use ratatui::widgets::TableState;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
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
    /// Full terminal shell mode
    Shell,
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
        source: DevcontainerSource,
    },
    /// Forget (untrack) a non-devc container without deleting it
    Forget {
        id: String,
        name: String,
    },
    /// Cancel an in-progress build/operation
    CancelBuild,
    /// Quit the application
    QuitApp,
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

/// A container operation in progress (shown as spinner modal)
#[derive(Debug, Clone)]
pub enum ContainerOperation {
    Starting { id: String, name: String },
    Stopping { id: String, name: String },
    Deleting { id: String, name: String },
    Up { id: String, name: String, progress: String },
}

impl ContainerOperation {
    pub fn label(&self) -> String {
        match self {
            ContainerOperation::Starting { name, .. } => format!("Starting {}...", name),
            ContainerOperation::Stopping { name, .. } => format!("Stopping {}...", name),
            ContainerOperation::Deleting { name, .. } => format!("Deleting {}...", name),
            ContainerOperation::Up { name, progress, .. } => {
                if progress.is_empty() {
                    format!("Starting up {}...", name)
                } else {
                    progress.clone()
                }
            }
        }
    }
}

/// Result of a background container operation
pub enum ContainerOpResult {
    Success(ContainerOperation),
    Failed(ContainerOperation, String),
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

/// Active shell session state (persistent across attach/detach cycles)
pub struct ShellSession {
    pub container_id: String,
    pub container_name: String,
    pub provider_container_id: String,
    pub provider_type: ProviderType,
    #[cfg(unix)]
    pub pty: Option<PtyShell>,
}

/// Application state
pub struct App {
    /// Container manager (wrapped in Arc<RwLock> for reconnection support)
    pub manager: Arc<RwLock<ContainerManager>>,
    /// Global configuration
    pub config: GlobalConfig,
    /// Workspace directory for auto-discovery
    pub workspace_dir: Option<std::path::PathBuf>,
    /// Last time auto-discovery was run (for debouncing)
    pub last_discovery: std::time::Instant,
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
    /// Whether socat is installed in the container (None = not checked yet)
    pub socat_installed: Option<bool>,
    /// Whether socat installation is in progress
    pub socat_installing: bool,
    /// Spinner frame for install animation
    pub spinner_frame: usize,
    /// Channel receiver for install result
    pub install_result_rx: Option<mpsc::UnboundedReceiver<InstallResult>>,

    // Port forwarder management (persists across views)
    /// Active port forwarders: (container_id, port) -> PortForwarder
    pub active_forwarders: HashMap<(String, u16), PortForwarder>,

    // Shell session state
    /// Persistent shell sessions keyed by container_id
    pub shell_sessions: HashMap<String, ShellSession>,
    /// Which container's shell is currently active (when View::Shell)
    pub active_shell_container: Option<String>,

    // Container operation spinner state
    /// Current container operation in progress (shown as spinner modal)
    pub container_op: Option<ContainerOperation>,
    /// Channel receiver for container operation results
    pub container_op_rx: Option<mpsc::UnboundedReceiver<ContainerOpResult>>,
    /// Channel receiver for container operation progress updates (e.g. Up steps)
    pub container_op_progress_rx: Option<mpsc::UnboundedReceiver<String>>,
    /// Captured output lines from initializeCommand (shown in spinner popup)
    pub up_output: Vec<String>,
    /// Channel receiver for initializeCommand output lines
    pub up_output_rx: Option<mpsc::UnboundedReceiver<String>>,

    // Compose service visibility state
    /// Cached compose service info keyed by devc container ID
    pub compose_services: HashMap<String, Vec<devc_provider::ComposeServiceInfo>>,
    /// Table state for compose services in detail view
    pub compose_services_table_state: TableState,
    /// Currently selected service index in compose services table
    pub compose_selected_service: usize,
    /// Whether compose services are currently being loaded
    pub compose_services_loading: bool,
    /// Name of the service whose logs are being viewed (None = primary container)
    pub logs_service_name: Option<String>,

    // Auto port forwarding state
    /// Background port detectors for auto-forwarding, keyed by provider container ID
    pub auto_port_detectors: HashMap<String, mpsc::UnboundedReceiver<PortDetectionUpdate>>,
    /// Auto-forward configurations per provider container ID
    pub auto_forward_configs: HashMap<String, Vec<devc_config::PortForwardConfig>>,
    /// Set of (provider_container_id, port) pairs that have been auto-forwarded
    pub auto_forwarded_ports: HashSet<(String, u16)>,
    /// Set of (provider_container_id, port) pairs where browser was already opened (for OpenBrowserOnce)
    pub auto_opened_ports: HashSet<(String, u16)>,
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
            workspace_dir: None,
            last_discovery: std::time::Instant::now(),
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
            socat_installed: None,
            socat_installing: false,
            spinner_frame: 0,
            install_result_rx: None,
            active_forwarders: HashMap::new(),
            shell_sessions: HashMap::new(),
            active_shell_container: None,
            container_op: None,
            container_op_rx: None,
            container_op_progress_rx: None,
            up_output: Vec::new(),
            up_output_rx: None,
            compose_services: HashMap::new(),
            compose_services_table_state: TableState::default(),
            compose_selected_service: 0,
            compose_services_loading: false,
            logs_service_name: None,
            auto_port_detectors: HashMap::new(),
            auto_forward_configs: HashMap::new(),
            auto_forwarded_ports: HashSet::new(),
            auto_opened_ports: HashSet::new(),
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
            compose_project: None,
            compose_service: None,
            source: DevcontainerSource::Devc,
        }
    }

    /// Create a test compose container state for testing
    ///
    /// Similar to `create_test_container` but with compose_project and compose_service set.
    pub fn create_test_compose_container(
        name: &str,
        status: DevcContainerStatus,
        project: &str,
        service: &str,
    ) -> ContainerState {
        use chrono::{TimeZone, Utc};
        use devc_provider::ProviderType;
        use std::collections::HashMap;
        use std::path::PathBuf;

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
            compose_project: Some(project.to_string()),
            compose_service: Some(service.to_string()),
            source: DevcontainerSource::Devc,
        }
    }

    /// Create a new application
    pub async fn new(manager: ContainerManager, workspace_dir: Option<&std::path::Path>) -> AppResult<Self> {
        let mut containers = manager.list().await?;

        // Append ephemeral Available entries for unregistered configs
        if let Some(dir) = workspace_dir {
            let unregistered = manager.find_unregistered_configs(dir).await;
            for (name, config_path, ws_path) in unregistered {
                containers.push(make_available_entry(name, config_path, ws_path));
            }
        }
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
            workspace_dir: workspace_dir.map(|p| p.to_path_buf()),
            last_discovery: std::time::Instant::now(),
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
            socat_installed: None,
            socat_installing: false,
            spinner_frame: 0,
            install_result_rx: None,
            active_forwarders: HashMap::new(),
            shell_sessions: HashMap::new(),
            active_shell_container: None,
            container_op: None,
            container_op_rx: None,
            container_op_progress_rx: None,
            up_output: Vec::new(),
            up_output_rx: None,
            compose_services: HashMap::new(),
            compose_services_table_state: TableState::default(),
            compose_selected_service: 0,
            compose_services_loading: false,
            logs_service_name: None,
            auto_port_detectors: HashMap::new(),
            auto_forward_configs: HashMap::new(),
            auto_forwarded_ports: HashSet::new(),
            auto_opened_ports: HashSet::new(),
        })
    }

    /// Check if connected to a container provider
    pub fn is_connected(&self) -> bool {
        self.active_provider.is_some()
    }

    /// Open the rebuild confirmation dialog for the selected container.
    /// Returns early if not connected or no containers.
    fn start_rebuild_dialog(&mut self) {
        if self.containers.is_empty() || !self.is_connected() {
            if !self.is_connected() {
                self.status_message = Some("Not connected to provider".to_string());
            }
            return;
        }
        let container = &self.containers[self.selected];
        if container.status.is_available() {
            self.status_message = Some("Use 'b' to build or 'u' to build and start".to_string());
            return;
        }
        if let Some(new_provider) = self.active_provider {
            let old_provider = container.provider;
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
        }
    }

    /// Build an Available (unregistered) entry: register it, then run build with log output.
    async fn build_available(&mut self) -> AppResult<()> {
        if self.containers.is_empty() || !self.is_connected() {
            if !self.is_connected() {
                self.status_message = Some("Not connected to provider".to_string());
            }
            return Ok(());
        }

        let container = &self.containers[self.selected];
        if !container.status.is_available() {
            self.status_message = Some("Already registered — use 'u' or 'R'".to_string());
            return Ok(());
        }

        // Register the Available entry
        let config_path = container.config_path.clone();
        let result = self.manager.read().await.init_from_config(&config_path).await;
        let id = match result {
            Ok(Some(cs)) => cs.id,
            Ok(None) => {
                self.status_message = Some("Already registered".to_string());
                self.refresh_containers().await?;
                return Ok(());
            }
            Err(e) => {
                self.status_message = Some(format!("Failed to register: {}", e));
                return Ok(());
            }
        };

        // Refresh so the registered entry replaces the ephemeral one
        self.refresh_containers().await?;
        if let Some(pos) = self.containers.iter().position(|c| c.id == id) {
            self.selected = pos;
            self.containers_table_state.select(Some(pos));
        }

        // Switch to build output view
        self.loading = true;
        self.view = View::BuildOutput;
        self.build_output.clear();
        self.build_output_scroll = 0;
        self.build_auto_scroll = true;
        self.build_complete = false;

        let (tx, rx) = mpsc::unbounded_channel();
        self.build_progress_rx = Some(rx);

        let manager = Arc::clone(&self.manager);
        tokio::spawn(async move {
            match manager.read().await.rebuild_with_progress(&id, false, tx.clone()).await {
                Ok(()) => {
                    // Success message is sent by rebuild_with_progress
                }
                Err(e) => {
                    let _ = tx.send(format!("Error: Build failed: {}", e));
                }
            }
        });

        Ok(())
    }

    /// Create a CliProvider for the given provider type.
    /// Handles toolbox environment detection for Podman.
    async fn create_cli_provider(
        provider_type: ProviderType,
    ) -> std::result::Result<devc_provider::CliProvider, devc_provider::ProviderError> {
        match provider_type {
            ProviderType::Docker => devc_provider::CliProvider::new_docker().await,
            ProviderType::Podman => {
                if devc_provider::is_in_toolbox() {
                    match devc_provider::CliProvider::new_toolbox().await {
                        Ok(p) => return Ok(p),
                        Err(_) => {} // Fall through to regular podman
                    }
                }
                devc_provider::CliProvider::new_podman().await
            }
        }
    }

    /// Run the application main loop
    pub async fn run<B: Backend + std::io::Write>(&mut self, terminal: &mut Terminal<B>) -> AppResult<()> {
        let mut events = Some(EventHandler::new(Duration::from_millis(250)));

        while !self.should_quit {
            // Handle shell mode specially - run shell session and return to TUI
            if self.view == View::Shell {
                #[cfg(unix)]
                {
                    self.run_shell_session(terminal, &mut events).await?;
                    continue; // Re-enter loop, will now draw TUI
                }
                #[cfg(not(unix))]
                {
                    self.view = View::Main;
                    continue;
                }
            }

            // Draw UI
            terminal.draw(|frame| ui::draw(frame, self))?;

            // Get event handler (should always be Some when not in shell mode)
            let handler = events.as_mut().expect("EventHandler missing outside shell mode");

            // Use select to handle multiple event sources for immediate updates
            tokio::select! {
                // Terminal/keyboard events
                event = handler.next() => {
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
                // Install result
                result = Self::recv_install_result(&mut self.install_result_rx) => {
                    if let Some(result) = result {
                        self.handle_install_result(result);
                    }
                }
                // Container operation result
                result = Self::recv_operation_result(&mut self.container_op_rx) => {
                    if let Some(result) = result {
                        self.handle_operation_result(result).await?;
                    }
                }
                // Container operation progress updates (e.g. Up steps)
                progress = Self::recv_op_progress(&mut self.container_op_progress_rx) => {
                    if let Some(msg) = progress {
                        if let Some(ref mut op) = self.container_op {
                            if let ContainerOperation::Up { progress, .. } = op {
                                *progress = msg;
                            }
                        }
                    }
                }
                // initializeCommand output lines
                line = Self::recv_up_output(&mut self.up_output_rx) => {
                    if let Some(line) = line {
                        self.up_output.push(line);
                    }
                }
            }
        }

        // Cleanup: stop all forwarders and shell sessions on exit
        for (_, forwarder) in self.active_forwarders.drain() {
            forwarder.stop().await;
        }
        self.shell_sessions.clear();

        Ok(())
    }

    /// Helper to receive from optional channel
    async fn recv_progress(rx: &mut Option<mpsc::UnboundedReceiver<String>>) -> Option<String> {
        match rx {
            Some(ref mut receiver) => receiver.recv().await,
            None => std::future::pending().await,
        }
    }

    /// Helper to receive container operation progress updates
    async fn recv_op_progress(rx: &mut Option<mpsc::UnboundedReceiver<String>>) -> Option<String> {
        match rx {
            Some(ref mut receiver) => receiver.recv().await,
            None => std::future::pending().await,
        }
    }

    /// Helper to receive initializeCommand output lines
    async fn recv_up_output(rx: &mut Option<mpsc::UnboundedReceiver<String>>) -> Option<String> {
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

    /// Ensure auto port detection is running for all running containers that declare ports.
    ///
    /// Called on each tick. For each running container with a provider container ID,
    /// if no detector exists yet, load its devcontainer config and if it declares ports
    /// via `auto_forward_config()`, spawn a port detector and cache the config.
    /// Remove detectors for containers that have stopped.
    async fn ensure_auto_port_detection(&mut self) {
        let running_container_ids: HashMap<String, String> = self
            .containers
            .iter()
            .filter(|c| c.status == DevcContainerStatus::Running)
            .filter_map(|c| {
                c.container_id
                    .as_ref()
                    .map(|cid| (cid.clone(), c.id.clone()))
            })
            .collect();

        // Remove detectors for containers that stopped
        let to_remove: Vec<String> = self
            .auto_port_detectors
            .keys()
            .filter(|cid| !running_container_ids.contains_key(*cid))
            .cloned()
            .collect();
        for cid in to_remove {
            self.auto_port_detectors.remove(&cid);
            self.auto_forward_configs.remove(&cid);
        }

        // Collect containers that need a new detector
        let needs_detector: Vec<(String, String)> = running_container_ids
            .iter()
            .filter(|(cid, _)| !self.auto_port_detectors.contains_key(*cid))
            .map(|(cid, did)| (cid.clone(), did.clone()))
            .collect();

        if needs_detector.is_empty() {
            return;
        }

        // Load configs while holding the manager lock, then release it
        let configs_to_start: Vec<(String, Vec<devc_config::PortForwardConfig>)> = {
            let manager = self.manager.read().await;
            let mut result = Vec::new();
            for (provider_cid, devc_id) in &needs_detector {
                let state = match manager.get(devc_id).await {
                    Ok(Some(s)) => s,
                    _ => continue,
                };
                let config = match manager.get_devcontainer_config(&state) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let auto_fwd = config.auto_forward_config();
                if !auto_fwd.is_empty() {
                    result.push((provider_cid.clone(), auto_fwd));
                }
            }
            result
        };

        // Now spawn detectors (no lock held)
        let provider_type = self.active_provider.unwrap_or(ProviderType::Docker);
        for (provider_cid, auto_fwd) in configs_to_start {
            // Create a new provider instance for the background detector task.
            // We use CliProvider directly (same pattern as existing port detection code).
            let provider_arc: Arc<dyn ContainerProvider + Send + Sync> = {
                match Self::create_cli_provider(provider_type).await {
                    Ok(p) => Arc::new(p),
                    Err(_) => continue,
                }
            };

            let container_id = devc_provider::ContainerId::new(&provider_cid);
            let forwarded: HashSet<u16> = self
                .active_forwarders
                .keys()
                .filter(|(cid, _)| cid == &provider_cid)
                .map(|(_, port)| *port)
                .collect();

            let rx = spawn_port_detector(provider_arc, container_id, provider_type, forwarded);
            self.auto_port_detectors.insert(provider_cid.clone(), rx);
            self.auto_forward_configs.insert(provider_cid, auto_fwd);
        }
    }

    /// Poll auto port detectors and auto-forward matching ports.
    ///
    /// Called on each tick. Drains updates from all detectors. When a detected port
    /// matches an auto-forward config entry (and action != Ignore, and not already
    /// forwarded), spawns a forwarder.
    async fn poll_auto_port_detectors(&mut self) {
        let provider_type = self.active_provider.unwrap_or(ProviderType::Docker);

        let cids: Vec<String> = self.auto_port_detectors.keys().cloned().collect();
        for cid in cids {
            let rx = match self.auto_port_detectors.get_mut(&cid) {
                Some(rx) => rx,
                None => continue,
            };

            // Drain all pending updates
            while let Ok(update) = rx.try_recv() {
                let config = match self.auto_forward_configs.get(&cid) {
                    Some(c) => c.clone(),
                    None => continue,
                };

                for detected in &update.ports {
                    for pfc in &config {
                        if detected.port != pfc.port {
                            continue;
                        }
                        if pfc.action == devc_config::AutoForwardAction::Ignore {
                            continue;
                        }
                        let key = (cid.clone(), pfc.port);
                        if self.auto_forwarded_ports.contains(&key) {
                            continue;
                        }
                        if self.active_forwarders.contains_key(&key) {
                            self.auto_forwarded_ports.insert(key);
                            continue;
                        }

                        // Auto-forward this port
                        match spawn_forwarder(provider_type, &cid, pfc.port, pfc.port).await {
                            Ok(forwarder) => {
                                self.active_forwarders.insert(key.clone(), forwarder);
                                self.auto_forwarded_ports.insert(key.clone());
                                match pfc.action {
                                    devc_config::AutoForwardAction::Notify => {
                                        let msg = if let Some(ref label) = pfc.label {
                                            format!(
                                                "Auto-forwarded port {} ({}) (localhost:{})",
                                                pfc.port, label, pfc.port
                                            )
                                        } else {
                                            format!(
                                                "Auto-forwarded port {} (localhost:{})",
                                                pfc.port, pfc.port
                                            )
                                        };
                                        self.status_message = Some(msg);
                                    }
                                    devc_config::AutoForwardAction::OpenBrowser => {
                                        let _ = open_in_browser(pfc.port, pfc.protocol.as_deref());
                                    }
                                    devc_config::AutoForwardAction::OpenBrowserOnce => {
                                        if !self.auto_opened_ports.contains(&key) {
                                            self.auto_opened_ports.insert(key);
                                            let _ = open_in_browser(pfc.port, pfc.protocol.as_deref());
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            Err(e) => {
                                tracing::debug!("Failed to auto-forward port {}: {}", pfc.port, e);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Fetch compose services for the currently selected container
    async fn fetch_compose_services(&mut self) {
        let container = match self.selected_container() {
            Some(c) => c.clone(),
            None => return,
        };

        // Only fetch for compose containers
        if container.compose_project.is_none() {
            return;
        }

        // Already cached
        if self.compose_services.contains_key(&container.id) {
            return;
        }

        self.compose_services_loading = true;

        // Load compose file paths from the devcontainer config
        let compose_info = {
            let core_container = match Container::from_config(&container.config_path) {
                Ok(c) => c,
                Err(_) => {
                    self.compose_services_loading = false;
                    return;
                }
            };
            let files = match core_container.compose_files() {
                Some(f) => f,
                None => {
                    self.compose_services_loading = false;
                    return;
                }
            };
            let project_name = core_container.compose_project_name();
            let workspace_path = core_container.workspace_path.clone();
            (files, project_name, workspace_path)
        };

        let (files, project_name, workspace_path) = compose_info;

        // Create a provider for the compose_ps call
        let provider_type = self.active_provider.unwrap_or(ProviderType::Docker);
        let provider = match Self::create_cli_provider(provider_type).await {
            Ok(p) => p,
            Err(_) => {
                self.compose_services_loading = false;
                return;
            }
        };

        let file_strs: Vec<String> = files.iter().map(|f| f.to_string_lossy().to_string()).collect();
        let file_refs: Vec<&str> = file_strs.iter().map(|s| s.as_str()).collect();

        match provider.compose_ps(&file_refs, &project_name, &workspace_path).await {
            Ok(services) => {
                self.compose_services.insert(container.id.clone(), services);
            }
            Err(_) => {
                // Store empty vec so we don't retry
                self.compose_services.insert(container.id.clone(), Vec::new());
            }
        }

        self.compose_services_loading = false;
    }

    /// Handle a single build progress message
    async fn handle_build_progress(&mut self, line: String) -> AppResult<()> {
        let is_complete = line.contains("complete") || line.contains("Error:") || line.contains("Failed:");
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
                // Advance spinner frame when installing or operating
                if self.socat_installing || self.container_op.is_some() {
                    self.spinner_frame = (self.spinner_frame + 1) % 10;
                }
                // Refresh container list periodically (only on Containers tab main view)
                if self.tab == Tab::Containers && self.view == View::Main && !self.loading {
                    self.refresh_containers().await?;
                }
                // Auto port forwarding: ensure detectors are running and poll for updates
                self.ensure_auto_port_detection().await;
                self.poll_auto_port_detectors().await;
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
        // Dismiss container operation spinner modal (Esc only)
        if self.container_op.is_some() && code == KeyCode::Esc {
            self.container_op = None;
            // Keep container_op_rx alive so result is still received
            self.status_message = Some("Operation continues in background...".to_string());
            return Ok(());
        }

        // Cancel install if in progress (before global Ctrl+C handler)
        if self.socat_installing
            && ((code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL))
                || code == KeyCode::Esc)
        {
            // Cancel install - drop the receiver, reset state
            self.install_result_rx = None;
            self.socat_installing = false;
            self.status_message = Some("Install cancelled".to_string());
            return Ok(());
        }

        // Ctrl+C shows quit confirmation dialog
        if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
            self.confirm_action = Some(ConfirmAction::QuitApp);
            self.view = View::Confirm;
            return Ok(());
        }

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
        if self.view == View::Main && self.tab == Tab::Containers && self.discover_mode
            && (code == KeyCode::Esc || code == KeyCode::Char('q'))
        {
            self.discover_mode = false;
            self.status_message = Some("Showing managed containers".to_string());
            return Ok(());
        }

        // View-specific exit handling (runs BEFORE global q/Esc)
        match (&self.view, code) {
            (View::Ports, KeyCode::Char('q') | KeyCode::Esc) => {
                self.exit_ports_view();
                return Ok(());
            }
            (View::ProviderDetail, KeyCode::Esc) if self.provider_detail_state.editing => {
                self.provider_detail_state.cancel_edit();
                return Ok(());
            }
            (View::BuildOutput, KeyCode::Char('q') | KeyCode::Esc) if self.build_complete => {
                self.build_output.clear();
                self.build_output_scroll = 0;
                self.build_complete = false;
                self.build_auto_scroll = true;
                self.view = View::Main;
                return Ok(());
            }
            _ => {}
        }

        // Global keys (work in any view)
        match code {
            KeyCode::Char('q') => {
                // Don't close BuildOutput view during active build
                if self.view == View::BuildOutput && !self.build_complete {
                    return Ok(());
                }
                if self.view != View::Main {
                    self.cleanup_view_state();
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
                    self.cleanup_view_state();
                    self.view = View::Main;
                }
                return Ok(());
            }
            KeyCode::Char('?') | KeyCode::F(1) => {
                self.view = View::Help;
                return Ok(());
            }
            // Tab switching with number keys (available in Main view and popup views)
            KeyCode::Char('1') if self.view == View::Main || self.is_popup_view() => {
                self.close_current_view();
                self.tab = Tab::Containers;
                return Ok(());
            }
            KeyCode::Char('2') if self.view == View::Main || self.is_popup_view() => {
                self.close_current_view();
                self.tab = Tab::Providers;
                return Ok(());
            }
            KeyCode::Char('3') if self.view == View::Main || self.is_popup_view() => {
                self.close_current_view();
                self.tab = Tab::Settings;
                return Ok(());
            }
            // Tab key cycles through tabs (in Main view and popup views)
            KeyCode::Tab if self.view == View::Main || self.is_popup_view() => {
                self.close_current_view();
                self.tab = match self.tab {
                    Tab::Containers => Tab::Providers,
                    Tab::Providers => Tab::Settings,
                    Tab::Settings => Tab::Containers,
                };
                return Ok(());
            }
            KeyCode::BackTab if self.view == View::Main || self.is_popup_view() => {
                self.close_current_view();
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
            View::Shell => {} // Shell mode is handled in run() before event loop
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
                                source: container.source.clone(),
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
                        self.compose_selected_service = 0;
                        self.compose_services_table_state.select(Some(0));
                        self.fetch_compose_services().await;
                    }
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
                        if container.status.is_available() {
                            self.status_message = Some("Not registered — nothing to remove".to_string());
                        } else {
                            self.confirm_action = Some(ConfirmAction::Delete(container.id.clone()));
                            self.dialog_focus = DialogFocus::Cancel;
                            self.view = View::Confirm;
                        }
                    }
                }
                KeyCode::Char('f') => {
                    if !self.containers.is_empty() {
                        let container = &self.containers[self.selected];
                        if container.source != DevcontainerSource::Devc && !container.status.is_available() {
                            self.confirm_action = Some(ConfirmAction::Forget {
                                id: container.id.clone(),
                                name: container.name.clone(),
                            });
                            self.dialog_focus = DialogFocus::Cancel;
                            self.view = View::Confirm;
                        } else if container.source == DevcontainerSource::Devc {
                            self.status_message = Some("Cannot forget devc-created containers".to_string());
                        }
                    }
                }
                KeyCode::Char('r') | KeyCode::F(5) => {
                    self.refresh_containers().await?;
                    self.status_message = Some("Refreshed".to_string());
                }
                KeyCode::Char('b') => {
                    self.build_available().await?;
                }
                KeyCode::Char('R') => {
                    self.start_rebuild_dialog();
                }
                KeyCode::Char('p') => {
                    // Enter port forwarding view for selected container
                    if !self.containers.is_empty() {
                        let container = self.containers[self.selected].clone();
                        self.enter_ports_view(&container).await?;
                    }
                }
                KeyCode::Char('S') => {
                    #[cfg(unix)]
                    {
                        // Enter shell mode for selected container
                        if !self.containers.is_empty() {
                            let container = self.containers[self.selected].clone();
                            self.enter_shell_mode(&container).await?;
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        self.status_message = Some("Shell not supported on this platform".to_string());
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
                        self.settings_state.saved = self.settings_state.draft.clone();
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

    /// Move compose service selection by delta (positive = down, negative = up)
    fn move_compose_service_selection(&mut self, delta: isize) {
        if let Some(container) = self.selected_container() {
            let container_id = container.id.clone();
            if let Some(services) = self.compose_services.get(&container_id) {
                if !services.is_empty() {
                    let len = services.len();
                    let current = self.compose_selected_service as isize;
                    self.compose_selected_service =
                        ((current + delta).rem_euclid(len as isize)) as usize;
                    self.compose_services_table_state
                        .select(Some(self.compose_selected_service));
                }
            }
        }
    }

    /// Handle container detail view keys
    async fn handle_detail_key(
        &mut self,
        code: KeyCode,
        _modifiers: KeyModifiers,
    ) -> AppResult<()> {
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_compose_service_selection(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_compose_service_selection(-1);
            }
            KeyCode::Char('r') | KeyCode::F(5) => {
                // Refresh: invalidate cached services and re-fetch
                if let Some(container) = self.selected_container() {
                    let id = container.id.clone();
                    self.compose_services.remove(&id);
                }
                self.fetch_compose_services().await;
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
            KeyCode::Char('b') => {
                self.build_available().await?;
            }
            KeyCode::Char('R') => {
                self.start_rebuild_dialog();
            }
            KeyCode::Char('S') => {
                #[cfg(unix)]
                {
                    // Enter shell mode for current container
                    if !self.containers.is_empty() {
                        let container = self.containers[self.selected].clone();
                        self.enter_shell_mode(&container).await?;
                    }
                }
                #[cfg(not(unix))]
                {
                    self.status_message = Some("Shell not supported on this platform".to_string());
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
                // Build complete case handled by view-specific exit above global keys.
                // Here we only handle the in-progress cancellation case.
                if !self.build_complete {
                    self.confirm_action = Some(ConfirmAction::CancelBuild);
                    self.view = View::Confirm;
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
                if self.socat_installed != Some(true) {
                    self.status_message = Some("socat required - press 'i' to install".to_string());
                } else if let Some(port) = self.detected_ports.get(self.selected_port) {
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
                if self.socat_installed != Some(true) {
                    self.status_message = Some("socat required - press 'i' to install".to_string());
                } else if let Some(port) = self.detected_ports.get(self.selected_port) {
                    if port.is_forwarded {
                        // Look up protocol from auto_forward_configs for this port
                        let protocol = self.ports_provider_container_id.as_ref().and_then(|cid| {
                            self.auto_forward_configs.get(cid).and_then(|configs| {
                                configs.iter().find(|c| c.port == port.port).and_then(|c| c.protocol.as_deref())
                            })
                        });
                        if let Err(e) = open_in_browser(port.port, protocol) {
                            self.status_message = Some(format!("Failed to open browser: {}", e));
                        }
                    } else {
                        self.status_message = Some("Port must be forwarded first".to_string());
                    }
                }
            }

            // Forward all
            KeyCode::Char('a') => {
                if self.socat_installed != Some(true) {
                    self.status_message = Some("socat required - press 'i' to install".to_string());
                } else {
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
            }

            // Stop all (none)
            KeyCode::Char('n') => {
                self.stop_all_forwards_for_container().await;
            }

            // Install socat
            KeyCode::Char('i') => {
                if self.socat_installed == Some(false) && !self.socat_installing {
                    self.install_socat_in_container();
                }
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
        self.socat_installed = None; // Will be checked below
        self.socat_installing = false;

        // Check if socat is installed
        let provider_type = self.active_provider.unwrap_or(ProviderType::Docker);
        let socat_check = check_socat_installed(provider_type, &provider_container_id).await;
        self.socat_installed = Some(socat_check);
        if !socat_check {
            self.status_message = Some("socat not installed - press 'i' to install".to_string());
        }

        // Get forwarded ports for this container
        let forwarded_ports: HashSet<u16> = self
            .active_forwarders
            .keys()
            .filter(|(cid, _)| cid == &provider_container_id)
            .map(|(_, port)| *port)
            .collect();

        // Start port detection polling - create a new provider instance for the background task
        let provider_type = self.active_provider.unwrap_or(ProviderType::Docker);
        let provider_result = Self::create_cli_provider(provider_type).await;

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
        self.socat_installed = None;
        self.socat_installing = false;
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

    /// Install socat in the current container (spawns background task)
    fn install_socat_in_container(&mut self) {
        let container_id = match &self.ports_provider_container_id {
            Some(id) => id.clone(),
            None => return,
        };

        let provider_type = self.active_provider.unwrap_or(ProviderType::Docker);

        // Create channel for result
        let (tx, rx) = mpsc::unbounded_channel();
        self.install_result_rx = Some(rx);
        self.socat_installing = true;
        self.spinner_frame = 0;
        self.status_message = Some("Installing socat...".to_string());

        // Spawn background task
        tokio::spawn(async move {
            let result = install_socat(provider_type, &container_id).await;
            let _ = tx.send(result);
        });
    }

    /// Handle install result from background task
    fn handle_install_result(&mut self, result: InstallResult) {
        self.socat_installing = false;
        self.install_result_rx = None;

        match result {
            InstallResult::Success => {
                self.socat_installed = Some(true);
                self.status_message = Some("socat installed successfully".to_string());
            }
            InstallResult::Failed(msg) => {
                self.status_message = Some(format!("Failed to install socat: {}", msg));
            }
            InstallResult::NoPackageManager => {
                self.status_message = Some("No supported package manager found in container".to_string());
            }
        }
    }

    /// Helper to receive install result
    async fn recv_install_result(
        rx: &mut Option<mpsc::UnboundedReceiver<InstallResult>>,
    ) -> Option<InstallResult> {
        match rx {
            Some(ref mut receiver) => receiver.recv().await,
            None => std::future::pending().await,
        }
    }

    /// Helper to receive container operation result
    async fn recv_operation_result(
        rx: &mut Option<mpsc::UnboundedReceiver<ContainerOpResult>>,
    ) -> Option<ContainerOpResult> {
        match rx {
            Some(ref mut receiver) => receiver.recv().await,
            None => std::future::pending().await,
        }
    }

    /// Handle container operation result from background task
    async fn handle_operation_result(&mut self, result: ContainerOpResult) -> AppResult<()> {
        self.container_op = None;
        self.container_op_rx = None;
        self.container_op_progress_rx = None;
        self.up_output_rx = None;
        self.up_output.clear();

        let affected_id = match &result {
            ContainerOpResult::Success(op) | ContainerOpResult::Failed(op, _) => {
                match op {
                    ContainerOperation::Starting { id, .. }
                    | ContainerOperation::Stopping { id, .. }
                    | ContainerOperation::Deleting { id, .. }
                    | ContainerOperation::Up { id, .. } => Some(id.clone()),
                }
            }
        };

        match result {
            ContainerOpResult::Success(op) => {
                let msg = match &op {
                    ContainerOperation::Starting { name, .. } => format!("Started {}", name),
                    ContainerOperation::Stopping { name, .. } => format!("Stopped {}", name),
                    ContainerOperation::Deleting { name, .. } => format!("Deleted {}", name),
                    ContainerOperation::Up { name, .. } => format!("Up completed for {}", name),
                };
                self.status_message = Some(msg);
            }
            ContainerOpResult::Failed(op, err) => {
                let msg = match &op {
                    ContainerOperation::Starting { name, .. } => format!("Start failed for {}: {}", name, err),
                    ContainerOperation::Stopping { name, .. } => format!("Stop failed for {}: {}", name, err),
                    ContainerOperation::Deleting { name, .. } => format!("Delete failed for {}: {}", name, err),
                    ContainerOperation::Up { name, .. } => format!("Up failed for {}: {}", name, err),
                };
                self.status_message = Some(msg);
            }
        }

        self.loading = false;
        self.refresh_containers().await?;

        // Invalidate cached compose services so status gets refreshed
        if let Some(id) = affected_id {
            self.compose_services.remove(&id);
        }
        // Re-fetch if still in detail view
        if self.view == View::ContainerDetail {
            self.fetch_compose_services().await;
        }

        Ok(())
    }

    /// Enter shell mode for a container
    #[cfg(unix)]
    async fn enter_shell_mode(&mut self, container: &ContainerState) -> AppResult<()> {
        if container.status != DevcContainerStatus::Running {
            self.status_message = Some("Container must be running to open shell".to_string());
            return Ok(());
        }

        let provider_container_id = match &container.container_id {
            Some(id) => id.clone(),
            None => {
                self.status_message = Some("Container has not been created yet".to_string());
                return Ok(());
            }
        };

        let container_id = container.id.clone();

        // Check if we already have a session for this container
        if let Some(session) = self.shell_sessions.get_mut(&container_id) {
            // Check if the PTY is still alive
            if session.pty.as_mut().is_some_and(|p| p.is_alive()) {
                // Reattach to existing session
                self.active_shell_container = Some(container_id);
                self.view = View::Shell;
                return Ok(());
            }
            // PTY is dead, remove the stale session - will create a new one below
            self.shell_sessions.remove(&container_id);
        }

        // Set up credential forwarding before spawning shell
        {
            let manager = self.manager.read().await;
            if let Err(e) = manager.setup_credentials_for_container(&container.id).await {
                tracing::warn!("Credential forwarding setup failed (non-fatal): {}", e);
            }
        }

        // Create a new session (PTY will be spawned in run_shell_session)
        self.shell_sessions.insert(
            container_id.clone(),
            ShellSession {
                container_id: container_id.clone(),
                container_name: container.name.clone(),
                provider_container_id,
                provider_type: self.active_provider.unwrap_or(container.provider),
                pty: None,
            },
        );

        // Fire-and-forget postAttachCommand for new sessions
        let manager = Arc::clone(&self.manager);
        let state_id = container.id.clone();
        tokio::spawn(async move {
            if let Err(e) = manager.read().await.run_post_attach_command(&state_id).await {
                tracing::warn!("postAttachCommand failed: {}", e);
            }
        });

        self.active_shell_container = Some(container_id);
        self.view = View::Shell;
        Ok(())
    }

    #[cfg(unix)]
    fn make_shell_config(&self, provider_type: ProviderType, container_id: String) -> ShellConfig {
        ShellConfig {
            provider_type,
            container_id,
            shell: self.config.defaults.shell.clone(),
            user: self.config.defaults.user.clone(),
            working_dir: None,
        }
    }

    /// Detect which shell is available in the container.
    /// Tests the configured shell first, falls back to /bin/sh.
    #[cfg(unix)]
    fn detect_shell(provider_type: ProviderType, container_id: &str, preferred: &str) -> String {
        let runtime = match provider_type {
            ProviderType::Docker => "docker",
            ProviderType::Podman => "podman",
        };

        let result = std::process::Command::new(runtime)
            .args(["exec", container_id, "test", "-x", preferred])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();

        if let Ok(status) = result {
            if status.success() {
                return preferred.to_string();
            }
        }

        "/bin/sh".to_string()
    }

    /// Run shell session - called from main loop when View::Shell
    ///
    /// Uses kill/recreate pattern for EventHandler instead of pause/resume.
    /// This prevents any keystroke competition and ensures clean state.
    #[cfg(unix)]
    async fn run_shell_session<B: Backend + std::io::Write>(
        &mut self,
        terminal: &mut Terminal<B>,
        events: &mut Option<EventHandler>,
    ) -> AppResult<()> {
        let container_id = match &self.active_shell_container {
            Some(id) => id.clone(),
            None => {
                self.view = View::Main;
                return Ok(());
            }
        };

        // Extract session info we need before taking the PTY
        let (container_name, provider_container_id, provider_type, has_pty) = {
            match self.shell_sessions.get(&container_id) {
                Some(s) => (
                    s.container_name.clone(),
                    s.provider_container_id.clone(),
                    s.provider_type,
                    s.pty.is_some(),
                ),
                None => {
                    self.view = View::Main;
                    self.active_shell_container = None;
                    return Ok(());
                }
            }
        };

        let is_reattach = has_pty;

        // 1. STOP event handler entirely (drop it)
        if let Some(mut handler) = events.take() {
            handler.stop();
        }

        // 2. Suspend TUI using terminal's backend
        suspend_tui(terminal.backend_mut())?;

        // 3. Reset terminal to sane state for shell
        crate::shell::reset_terminal();

        // 4. Show entry message (first attach only)
        if !is_reattach {
            println!(
                "\nShell for '{}' (Ctrl+\\ to detach, session preserved)\n",
                container_name
            );
        }

        // 5. Get or spawn PtyShell
        // Take the existing PTY out of the session (if any)
        let existing_pty = self
            .shell_sessions
            .get_mut(&container_id)
            .and_then(|s| s.pty.take());

        let mut pty = match existing_pty {
            Some(mut p) => {
                if !p.is_alive() {
                    // PTY died while we were away, spawn a new one below
                    drop(p);
                    let mut config = self.make_shell_config(provider_type, provider_container_id.clone());
                    config.shell = Self::detect_shell(provider_type, &provider_container_id, &config.shell);
                    match PtyShell::spawn(&config) {
                        Ok(new_p) => new_p,
                        Err(e) => {
                            self.status_message = Some(format!("Shell spawn error: {}", e));
                            self.shell_sessions.remove(&container_id);
                            self.active_shell_container = None;
                            self.view = View::Main;
                            crate::shell::reset_terminal();
                            resume_tui(terminal.backend_mut())?;
                            *events = Some(EventHandler::new(Duration::from_millis(250)));
                            terminal.clear()?;
                            return Ok(());
                        }
                    }
                } else {
                    // Reattach: restore alternate screen if child app was using it
                    if p.is_in_alternate_screen() {
                        let _ = std::io::Write::write_all(
                            &mut std::io::stdout(),
                            b"\x1b[?1049h",
                        );
                        let _ = std::io::Write::flush(&mut std::io::stdout());
                    }
                    p
                }
            }
            _ => {
                // Spawn new PTY
                let mut config = self.make_shell_config(provider_type, provider_container_id.clone());
                config.shell = Self::detect_shell(provider_type, &provider_container_id, &config.shell);
                match PtyShell::spawn(&config) {
                    Ok(p) => p,
                    Err(e) => {
                        self.status_message = Some(format!("Shell spawn error: {}", e));
                        self.shell_sessions.remove(&container_id);
                        self.active_shell_container = None;
                        self.view = View::Main;
                        crate::shell::reset_terminal();
                        resume_tui(terminal.backend_mut())?;
                        *events = Some(EventHandler::new(Duration::from_millis(250)));
                        terminal.clear()?;
                        return Ok(());
                    }
                }
            }
        };

        // 6. Run relay in spawn_blocking (returns PtyShell + reason)
        let relay_result = tokio::task::spawn_blocking(move || {
            let reason = pty.relay(is_reattach);
            (pty, reason)
        })
        .await;

        // 7. Process result
        match relay_result {
            Ok((pty, reason)) => match reason {
                ShellExitReason::Detached => {
                    let was_alt = pty.is_in_alternate_screen();
                    // Set dummy size so next reattach guarantees a real size change
                    // (docker exec only propagates SIGWINCH when size actually differs)
                    pty.set_size_and_signal(1, 1);
                    // Put PTY back into session - session preserved
                    if let Some(session) = self.shell_sessions.get_mut(&container_id) {
                        session.pty = Some(pty);
                    }
                    // Leave child's alternate screen before entering TUI's
                    if was_alt {
                        let _ = std::io::Write::write_all(
                            &mut std::io::stdout(),
                            b"\x1b[?1049l",
                        );
                        let _ = std::io::Write::flush(&mut std::io::stdout());
                    }
                    self.status_message = Some(format!(
                        "Detached from '{}' (session preserved, press S to reattach)",
                        container_name
                    ));
                }
                ShellExitReason::Exited => {
                    // Shell exited - clean up session
                    drop(pty);
                    self.shell_sessions.remove(&container_id);
                    self.status_message = Some("Shell exited".to_string());
                }
                ShellExitReason::Error(e) => {
                    drop(pty);
                    self.shell_sessions.remove(&container_id);
                    self.status_message = Some(format!("Shell error: {}", e));
                }
            },
            Err(e) => {
                // Lost the PtyShell — clean up session and recover
                self.shell_sessions.remove(&container_id);
                self.status_message = Some(format!("Shell error: {}", e));
            }
        }

        // 8. Reset terminal before resuming TUI
        crate::shell::reset_terminal();

        // 9. Return to main view
        self.active_shell_container = None;
        self.view = View::Main;

        // 10. Resume TUI using terminal's backend
        resume_tui(terminal.backend_mut())?;

        // 11. Create FRESH event handler
        *events = Some(EventHandler::new(Duration::from_millis(250)));

        // 12. Force terminal to redraw everything
        terminal.clear()?;

        Ok(())
    }

    /// Refresh container list
    async fn refresh_containers(&mut self) -> AppResult<()> {
        self.containers = self.manager.read().await.list().await?;

        // Sync status for all registered containers
        for container in &self.containers {
            let _ = self.manager.read().await.sync_status(&container.id).await;
        }

        // Re-fetch after sync
        self.containers = self.manager.read().await.list().await?;

        // Append ephemeral Available entries for unregistered configs
        if let Some(ref dir) = self.workspace_dir {
            let unregistered = self.manager.read().await.find_unregistered_configs(dir).await;
            for (name, config_path, ws_path) in unregistered {
                self.containers.push(make_available_entry(name, config_path, ws_path));
            }
        }

        // Ensure selected index is valid
        if !self.containers.is_empty() && self.selected >= self.containers.len() {
            self.selected = self.containers.len() - 1;
        }
        // Sync table state
        if !self.containers.is_empty() {
            self.containers_table_state.select(Some(self.selected));
        }

        // Invalidate stale compose_services entries for containers that no longer exist
        let container_ids: HashSet<String> = self.containers.iter().map(|c| c.id.clone()).collect();
        self.compose_services.retain(|id, _| container_ids.contains(id));

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

    /// Toggle start/stop for selected container (background task with spinner)
    async fn toggle_selected(&mut self) -> AppResult<()> {
        if self.containers.is_empty() || self.container_op.is_some() {
            return Ok(());
        }

        let container = &self.containers[self.selected];
        if container.status.is_available() {
            self.status_message = Some("Use 'u' to build first".to_string());
            return Ok(());
        }
        let id = container.id.clone();
        let name = container.name.clone();

        let op = match container.status {
            DevcContainerStatus::Running => ContainerOperation::Stopping { id: id.clone(), name: name.clone() },
            DevcContainerStatus::Stopped | DevcContainerStatus::Created => ContainerOperation::Starting { id: id.clone(), name: name.clone() },
            _ => {
                self.status_message = Some("Cannot start/stop in current state".to_string());
                return Ok(());
            }
        };

        let is_start = matches!(op, ContainerOperation::Starting { .. });
        self.container_op = Some(op.clone());
        self.loading = true;
        self.spinner_frame = 0;

        let (tx, rx) = mpsc::unbounded_channel();
        self.container_op_rx = Some(rx);

        let manager = Arc::clone(&self.manager);
        tokio::spawn(async move {
            if is_start {
                match manager.read().await.start(&id).await {
                    Ok(()) => { let _ = tx.send(ContainerOpResult::Success(op)); }
                    Err(e) => { let _ = tx.send(ContainerOpResult::Failed(op, e.to_string())); }
                }
            } else {
                match manager.read().await.stop(&id).await {
                    Ok(()) => { let _ = tx.send(ContainerOpResult::Success(op)); }
                    Err(e) => { let _ = tx.send(ContainerOpResult::Failed(op, e.to_string())); }
                }
            }
        });

        Ok(())
    }

    /// Run full up (build, create, start) for selected container
    async fn up_selected(&mut self) -> AppResult<()> {
        if self.containers.is_empty() || self.container_op.is_some() {
            return Ok(());
        }

        // If this is an Available (unregistered) entry, register it first
        let is_available = self.containers[self.selected].status.is_available();
        if is_available {
            let config_path = self.containers[self.selected].config_path.clone();
            let result = self.manager.read().await.init_from_config(&config_path).await;
            match result {
                Ok(Some(_cs)) => {
                    // Refresh so the new registered entry replaces the ephemeral one
                    self.refresh_containers().await?;
                    // Find the newly registered entry by its config_path
                    if let Some(pos) = self.containers.iter().position(|c| c.config_path == config_path && !c.status.is_available()) {
                        self.selected = pos;
                        self.containers_table_state.select(Some(pos));
                    }
                    // Fall through to the normal up logic below
                }
                Ok(None) => {
                    // Already registered (race condition), just refresh
                    self.refresh_containers().await?;
                    return Ok(());
                }
                Err(e) => {
                    self.status_message = Some(format!("Failed to register: {}", e));
                    return Ok(());
                }
            }
        }

        let container = &self.containers[self.selected];
        let id = container.id.clone();
        let name = container.name.clone();

        let op = ContainerOperation::Up {
            id: id.clone(),
            name: name.clone(),
            progress: format!("Starting up {}...", name),
        };
        self.container_op = Some(op.clone());
        self.loading = true;
        self.spinner_frame = 0;

        let (result_tx, result_rx) = mpsc::unbounded_channel();
        self.container_op_rx = Some(result_rx);

        let (progress_tx, progress_rx) = mpsc::unbounded_channel();
        self.container_op_progress_rx = Some(progress_rx);

        let (output_tx, output_rx) = mpsc::unbounded_channel();
        self.up_output = Vec::new();
        self.up_output_rx = Some(output_rx);

        let manager = Arc::clone(&self.manager);
        tokio::spawn(async move {
            match manager.read().await.up_with_progress(&id, Some(&progress_tx), Some(&output_tx)).await {
                Ok(()) => { let _ = result_tx.send(ContainerOpResult::Success(op)); }
                Err(e) => { let _ = result_tx.send(ContainerOpResult::Failed(op, e.to_string())); }
            }
        });

        Ok(())
    }

    /// Fetch logs for the selected container or companion service
    async fn fetch_logs(&mut self) -> AppResult<()> {
        if self.containers.is_empty() {
            return Ok(());
        }

        let container = &self.containers[self.selected];

        // Check if we should fetch logs for a companion service
        let companion = if self.view == View::ContainerDetail {
            let container_id = container.id.clone();
            self.compose_services.get(&container_id).and_then(|services| {
                services.get(self.compose_selected_service).and_then(|svc| {
                    // If this is the primary service, use normal log path
                    let is_primary = container.compose_service.as_deref() == Some(&svc.service_name);
                    if is_primary {
                        None
                    } else {
                        Some((svc.container_id.clone(), svc.service_name.clone()))
                    }
                })
            })
        } else {
            None
        };

        if let Some((svc_container_id, svc_name)) = companion {
            // Fetch logs directly from the provider for the companion service
            self.status_message = Some(format!("Loading logs for {}...", svc_name));
            self.loading = true;

            let provider_type = self.active_provider.unwrap_or(ProviderType::Docker);
            match Self::create_cli_provider(provider_type).await {
                Ok(provider) => {
                    let log_config = devc_provider::LogConfig {
                        follow: false,
                        stdout: true,
                        stderr: true,
                        tail: Some(1000),
                        timestamps: false,
                        since: None,
                        until: None,
                    };
                    match provider.logs(&svc_container_id, &log_config).await {
                        Ok(log_stream) => {
                            use tokio::io::AsyncBufReadExt;
                            let reader = tokio::io::BufReader::new(log_stream.stream);
                            let mut lines_reader = reader.lines();
                            let mut lines = Vec::new();
                            while let Ok(Some(line)) = lines_reader.next_line().await {
                                lines.push(line);
                            }
                            self.logs = lines;
                            self.logs_scroll = self.logs.len().saturating_sub(1);
                            self.logs_service_name = Some(svc_name);
                            self.view = View::Logs;
                            self.status_message = Some(format!("{} log lines", self.logs.len()));
                        }
                        Err(e) => {
                            self.status_message = Some(format!("Failed to fetch logs: {}", e));
                        }
                    }
                }
                Err(e) => {
                    self.status_message = Some(format!("Failed to create provider: {}", e));
                }
            }

            self.loading = false;
            return Ok(());
        }

        // Normal path: fetch logs for the primary container
        if container.container_id.is_none() {
            self.status_message = Some("Container has not been created yet".to_string());
            return Ok(());
        }

        self.status_message = Some(format!("Loading logs for {}...", container.name));
        self.loading = true;
        self.logs_service_name = None;

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
                if self.container_op.is_some() {
                    return Ok(());
                }
                // Clean up any shell session for this container
                self.shell_sessions.remove(&id);

                let name = self.containers.iter()
                    .find(|c| c.id == id)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| id.clone());

                let op = ContainerOperation::Deleting { id: id.clone(), name };
                self.container_op = Some(op.clone());
                self.loading = true;
                self.spinner_frame = 0;

                let (tx, rx) = mpsc::unbounded_channel();
                self.container_op_rx = Some(rx);

                let manager = Arc::clone(&self.manager);
                tokio::spawn(async move {
                    match manager.read().await.remove(&id, true).await {
                        Ok(()) => { let _ = tx.send(ContainerOpResult::Success(op)); }
                        Err(e) => { let _ = tx.send(ContainerOpResult::Failed(op, e.to_string())); }
                    }
                });
            }
            ConfirmAction::Stop(id) => {
                if self.container_op.is_some() {
                    return Ok(());
                }
                // Clean up any shell session for this container
                self.shell_sessions.remove(&id);

                let name = self.containers.iter()
                    .find(|c| c.id == id)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| id.clone());

                let op = ContainerOperation::Stopping { id: id.clone(), name };
                self.container_op = Some(op.clone());
                self.loading = true;
                self.spinner_frame = 0;

                let (tx, rx) = mpsc::unbounded_channel();
                self.container_op_rx = Some(rx);

                let manager = Arc::clone(&self.manager);
                tokio::spawn(async move {
                    match manager.read().await.stop(&id).await {
                        Ok(()) => { let _ = tx.send(ContainerOpResult::Success(op)); }
                        Err(e) => { let _ = tx.send(ContainerOpResult::Failed(op, e.to_string())); }
                    }
                });
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
            ConfirmAction::Adopt { container_id, container_name, workspace_path, source } => {
                self.loading = true;
                self.status_message = Some(format!("Adopting '{}'...", container_name));

                // Use a block to ensure the read guard is dropped before refresh_containers
                let adopt_result = {
                    let manager = self.manager.read().await;
                    manager.adopt(&container_id, workspace_path.as_deref(), source).await
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
            ConfirmAction::Forget { id, name } => {
                self.loading = true;
                self.status_message = Some(format!("Forgetting '{}'...", name));

                let forget_result = {
                    let manager = self.manager.read().await;
                    manager.forget(&id).await
                };

                match forget_result {
                    Ok(()) => {
                        self.status_message = Some(format!("Forgot '{}' (container still running)", name));
                        self.refresh_containers().await?;
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Failed to forget: {}", e));
                    }
                }
                self.loading = false;
            }
            ConfirmAction::CancelBuild => {
                // Cancel the in-progress build and return to main view
                self.build_progress_rx = None; // Drop the receiver, which stops the build task
                self.loading = false;
                self.build_complete = false;
                self.build_output.clear();
                self.build_output_scroll = 0;
                self.build_auto_scroll = true;
                self.view = View::Main;
                self.status_message = Some("Build cancelled".to_string());
                self.refresh_containers().await?;
            }
            ConfirmAction::QuitApp => {
                self.should_quit = true;
            }
        }
        Ok(())
    }

    /// Get the currently selected container
    pub fn selected_container(&self) -> Option<&ContainerState> {
        self.containers.get(self.selected)
    }

    /// Check if the current view is a popup overlay
    fn is_popup_view(&self) -> bool {
        matches!(
            self.view,
            View::ContainerDetail | View::ProviderDetail | View::Ports | View::Logs
        )
    }

    /// Clean up view-specific state when leaving any view
    fn cleanup_view_state(&mut self) {
        match self.view {
            View::ContainerDetail => {
                self.compose_selected_service = 0;
                self.compose_services_table_state = TableState::default();
                self.compose_services_loading = false;
            }
            View::Logs => {
                self.logs_service_name = None;
            }
            _ => {}
        }
    }

    /// Close the current popup view with proper cleanup
    fn close_current_view(&mut self) {
        match self.view {
            View::Ports => {
                self.ports_container_id = None;
                self.ports_provider_container_id = None;
                self.port_detect_rx = None;
                self.detected_ports.clear();
                self.socat_installed = None;
                self.socat_installing = false;
            }
            View::ProviderDetail if self.provider_detail_state.editing => {
                self.provider_detail_state.cancel_edit();
            }
            _ => {}
        }
        self.cleanup_view_state();
        self.view = View::Main;
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

/// Build an ephemeral ContainerState for an unregistered config.
/// Uses a deterministic ID derived from the config path so it stays
/// stable across refreshes.
fn make_available_entry(
    name: String,
    config_path: PathBuf,
    workspace_path: PathBuf,
) -> ContainerState {
    use std::collections::hash_map::DefaultHasher;

    let mut hasher = DefaultHasher::new();
    config_path.hash(&mut hasher);
    let id = format!("avail-{:x}", hasher.finish());

    let now = chrono::Utc::now();
    ContainerState {
        id,
        name,
        provider: ProviderType::Docker, // placeholder — not meaningful for Available
        config_path,
        image_id: None,
        container_id: None,
        status: DevcContainerStatus::Available,
        created_at: now,
        last_used: now,
        workspace_path,
        metadata: std::collections::HashMap::new(),
        compose_project: None,
        compose_service: None,
        source: DevcontainerSource::Devc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devc_provider::{ComposeServiceInfo, ContainerId, ContainerStatus};

    #[test]
    fn test_compose_service_selection_forward_wraps() {
        let mut app = App::new_for_testing();
        let container =
            App::create_test_compose_container("myapp", DevcContainerStatus::Running, "proj", "app");
        let cid = container.id.clone();
        app.containers.push(container);
        app.selected = 0;

        // Insert 3 services for this container
        app.compose_services.insert(
            cid.clone(),
            vec![
                ComposeServiceInfo {
                    service_name: "app".to_string(),
                    container_id: ContainerId::new("c1"),
                    status: ContainerStatus::Running,
                },
                ComposeServiceInfo {
                    service_name: "db".to_string(),
                    container_id: ContainerId::new("c2"),
                    status: ContainerStatus::Running,
                },
                ComposeServiceInfo {
                    service_name: "redis".to_string(),
                    container_id: ContainerId::new("c3"),
                    status: ContainerStatus::Running,
                },
            ],
        );

        assert_eq!(app.compose_selected_service, 0);
        app.move_compose_service_selection(1); // 0 -> 1
        assert_eq!(app.compose_selected_service, 1);
        app.move_compose_service_selection(1); // 1 -> 2
        assert_eq!(app.compose_selected_service, 2);
        app.move_compose_service_selection(1); // 2 -> 0 (wraps)
        assert_eq!(app.compose_selected_service, 0);
        app.move_compose_service_selection(1); // 0 -> 1
        assert_eq!(app.compose_selected_service, 1);
    }

    #[test]
    fn test_compose_service_selection_backward_wraps() {
        let mut app = App::new_for_testing();
        let container =
            App::create_test_compose_container("myapp", DevcContainerStatus::Running, "proj", "app");
        let cid = container.id.clone();
        app.containers.push(container);
        app.selected = 0;

        app.compose_services.insert(
            cid.clone(),
            vec![
                ComposeServiceInfo {
                    service_name: "app".to_string(),
                    container_id: ContainerId::new("c1"),
                    status: ContainerStatus::Running,
                },
                ComposeServiceInfo {
                    service_name: "db".to_string(),
                    container_id: ContainerId::new("c2"),
                    status: ContainerStatus::Running,
                },
                ComposeServiceInfo {
                    service_name: "redis".to_string(),
                    container_id: ContainerId::new("c3"),
                    status: ContainerStatus::Running,
                },
            ],
        );

        assert_eq!(app.compose_selected_service, 0);
        app.move_compose_service_selection(-1); // 0 -> 2 (wraps backward)
        assert_eq!(app.compose_selected_service, 2);
        app.move_compose_service_selection(-1); // 2 -> 1
        assert_eq!(app.compose_selected_service, 1);
    }

    #[test]
    fn test_compose_service_selection_empty_noop() {
        let mut app = App::new_for_testing();
        let container =
            App::create_test_compose_container("myapp", DevcContainerStatus::Running, "proj", "app");
        app.containers.push(container);
        app.selected = 0;
        // No services in compose_services map

        app.compose_selected_service = 0;
        app.move_compose_service_selection(1);
        assert_eq!(app.compose_selected_service, 0);
        app.move_compose_service_selection(-1);
        assert_eq!(app.compose_selected_service, 0);
    }
}
