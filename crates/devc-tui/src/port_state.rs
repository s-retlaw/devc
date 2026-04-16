//! Port forwarding state extracted from App

use crate::ports::DetectedPort;
use crate::ports::PortDetectionUpdate;
use crate::tunnel::{spawn_forwarder, PortForwarder};
use ratatui::widgets::TableState;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// All port-forwarding state, both per-view and persistent across views.
pub struct PortForwardingState {
    // === Per-view state (cleared when exiting ports view) ===
    /// Container currently being viewed for port forwarding (devc container ID)
    pub container_id: Option<String>,
    /// Provider container ID for the ports view
    pub provider_container_id: Option<String>,
    /// Runtime program for the current ports view container
    pub runtime_program: Option<String>,
    /// Runtime prefix args for the current ports view container
    pub runtime_prefix: Vec<String>,
    /// Detected ports in current container
    pub detected_ports: Vec<DetectedPort>,
    /// Selected port index
    pub selected_port: usize,
    /// Table state for port list
    pub table_state: TableState,
    /// Whether socat is installed in the container (None = not checked yet)
    pub socat_installed: Option<bool>,
    /// Whether socat installation is in progress
    pub socat_installing: bool,
    /// Handle for the active port detection task (aborted when ports view is closed)
    pub port_detect_handle: Option<tokio::task::JoinHandle<()>>,

    // === Persistent state (survives view changes) ===
    /// Active port forwarders: (container_id, port) -> PortForwarder
    pub active_forwarders: HashMap<(String, u16), PortForwarder>,

    // === Auto port forwarding state ===
    /// Background port detectors for auto-forwarding, keyed by provider container ID
    pub auto_port_detectors: HashMap<String, mpsc::UnboundedReceiver<PortDetectionUpdate>>,
    /// Auto-forward configurations per provider container ID
    pub auto_forward_configs: HashMap<String, Vec<devc_config::PortForwardConfig>>,
    /// Set of (provider_container_id, port) pairs that have been auto-forwarded
    pub auto_forwarded_ports: HashSet<(String, u16)>,
    /// Set of (provider_container_id, port) pairs where browser was already opened (for OpenBrowserOnce)
    pub auto_opened_ports: HashSet<(String, u16)>,
    /// Cached runtime args per provider container ID (for auto-forwarding)
    pub auto_runtime_args: HashMap<String, (String, Vec<String>)>,
    /// Containers with auto-forward-all enabled (provider container IDs)
    pub auto_forward_all_containers: HashSet<String>,
}

impl PortForwardingState {
    /// Create a new default port forwarding state
    pub fn new() -> Self {
        Self {
            container_id: None,
            provider_container_id: None,
            runtime_program: None,
            runtime_prefix: Vec::new(),
            detected_ports: Vec::new(),
            selected_port: 0,
            table_state: TableState::default().with_selected(0),
            socat_installed: None,
            socat_installing: false,
            port_detect_handle: None,
            active_forwarders: HashMap::new(),
            auto_port_detectors: HashMap::new(),
            auto_forward_configs: HashMap::new(),
            auto_forwarded_ports: HashSet::new(),
            auto_opened_ports: HashSet::new(),
            auto_runtime_args: HashMap::new(),
            auto_forward_all_containers: HashSet::new(),
        }
    }

    /// Handle a port detection update (updates detected_ports list)
    pub fn handle_port_update(&mut self, update: PortDetectionUpdate) {
        let forwarded_ports: HashSet<u16> =
            if let Some(ref container_id) = self.provider_container_id {
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

        if !self.detected_ports.is_empty() && self.selected_port >= self.detected_ports.len() {
            self.selected_port = self.detected_ports.len() - 1;
        }
        if !self.detected_ports.is_empty() {
            self.table_state.select(Some(self.selected_port));
        }
    }

    /// Move selection to the next port (wrapping)
    pub fn select_next(&mut self) {
        if !self.detected_ports.is_empty() {
            self.selected_port = (self.selected_port + 1) % self.detected_ports.len();
            self.table_state.select(Some(self.selected_port));
        }
    }

    /// Move selection to the previous port (wrapping)
    pub fn select_prev(&mut self) {
        if !self.detected_ports.is_empty() {
            self.selected_port = self
                .selected_port
                .checked_sub(1)
                .unwrap_or(self.detected_ports.len() - 1);
            self.table_state.select(Some(self.selected_port));
        }
    }

    /// Move selection to the first port
    pub fn select_first(&mut self) {
        if !self.detected_ports.is_empty() {
            self.selected_port = 0;
            self.table_state.select(Some(0));
        }
    }

    /// Move selection to the last port
    pub fn select_last(&mut self) {
        if !self.detected_ports.is_empty() {
            self.selected_port = self.detected_ports.len() - 1;
            self.table_state.select(Some(self.selected_port));
        }
    }

    /// Get the currently selected port info
    pub fn selected_port_info(&self) -> Option<&DetectedPort> {
        self.detected_ports.get(self.selected_port)
    }

    /// Initialize per-view state for entering the ports view
    pub fn enter_view(
        &mut self,
        container_id: String,
        provider_id: String,
        program: String,
        prefix: Vec<String>,
    ) {
        self.container_id = Some(container_id);
        self.provider_container_id = Some(provider_id);
        self.runtime_program = Some(program);
        self.runtime_prefix = prefix;
        self.detected_ports.clear();
        self.selected_port = 0;
        self.table_state.select(Some(0));
        self.socat_installed = None;
        self.socat_installing = false;
    }

    /// Clear per-view state (called when exiting ports view)
    pub fn clear_view_state(&mut self) {
        self.container_id = None;
        self.provider_container_id = None;
        self.runtime_program = None;
        self.runtime_prefix.clear();
        if let Some(handle) = self.port_detect_handle.take() {
            handle.abort();
        }
        self.detected_ports.clear();
        self.socat_installed = None;
        self.socat_installing = false;
    }

    /// Extract auto-forwarding state for a background task (used during shell sessions).
    /// After this call, the auto-forwarding fields on self are empty.
    pub fn take_auto_forward_state(
        &mut self,
        auto_forward_global: bool,
        auto_open_browser_global: bool,
    ) -> ShellAutoForwardState {
        ShellAutoForwardState {
            detectors: std::mem::take(&mut self.auto_port_detectors),
            configs: std::mem::take(&mut self.auto_forward_configs),
            runtime_args: std::mem::take(&mut self.auto_runtime_args),
            forwarded_ports: std::mem::take(&mut self.auto_forwarded_ports),
            opened_ports: std::mem::take(&mut self.auto_opened_ports),
            forwarders: std::mem::take(&mut self.active_forwarders),
            auto_forward_all: self.auto_forward_all_containers.clone(),
            auto_forward_all_global: auto_forward_global,
            auto_open_browser_global,
        }
    }

    /// Merge auto-forwarding state back from a background task.
    pub fn restore_auto_forward_state(&mut self, state: ShellAutoForwardState) {
        self.auto_port_detectors = state.detectors;
        self.auto_forward_configs = state.configs;
        self.auto_runtime_args = state.runtime_args;
        self.auto_forwarded_ports = state.forwarded_ports;
        self.auto_opened_ports = state.opened_ports;
        self.active_forwarders = state.forwarders;
        // auto_forward_all_containers stays on self (clone, not take)
    }
}

impl Default for PortForwardingState {
    fn default() -> Self {
        Self::new()
    }
}

/// State extracted from PortForwardingState for background auto-forwarding during shell sessions.
pub struct ShellAutoForwardState {
    pub detectors: HashMap<String, mpsc::UnboundedReceiver<PortDetectionUpdate>>,
    pub configs: HashMap<String, Vec<devc_config::PortForwardConfig>>,
    pub runtime_args: HashMap<String, (String, Vec<String>)>,
    pub forwarded_ports: HashSet<(String, u16)>,
    pub opened_ports: HashSet<(String, u16)>,
    pub forwarders: HashMap<(String, u16), PortForwarder>,
    pub auto_forward_all: HashSet<String>,
    pub auto_forward_all_global: bool,
    pub auto_open_browser_global: bool,
}

/// Poll auto port detectors in background mode (no TUI actions).
/// Shell-session equivalent of App::poll_auto_port_detectors().
async fn poll_detectors_background(state: &mut ShellAutoForwardState) {
    let cids: Vec<String> = state.detectors.keys().cloned().collect();
    for cid in cids {
        let rx = match state.detectors.get_mut(&cid) {
            Some(rx) => rx,
            None => continue,
        };

        while let Ok(update) = rx.try_recv() {
            let is_auto_all = state.auto_forward_all.contains(&cid);
            let config = state.configs.get(&cid).cloned().unwrap_or_default();

            for detected in &update.ports {
                // Check if this port matches a config entry
                let matching_config = config.iter().find(|pfc| pfc.port == detected.port);

                // Determine if we should forward this port
                let should_forward = if let Some(pfc) = matching_config {
                    pfc.action != devc_config::AutoForwardAction::Ignore
                } else {
                    is_auto_all || state.auto_forward_all_global
                };

                if !should_forward {
                    continue;
                }

                let key = (cid.clone(), detected.port);
                if state.forwarded_ports.contains(&key) {
                    continue;
                }
                if state.forwarders.contains_key(&key) {
                    state.forwarded_ports.insert(key);
                    continue;
                }

                let (rt_prog, rt_prefix) = state
                    .runtime_args
                    .get(&cid)
                    .cloned()
                    .unwrap_or_else(|| ("docker".to_string(), vec![]));

                match spawn_forwarder(
                    rt_prog,
                    rt_prefix,
                    cid.clone(),
                    detected.port,
                    detected.port,
                )
                .await
                {
                    Ok(forwarder) => {
                        state.forwarders.insert(key.clone(), forwarder);
                        state.forwarded_ports.insert(key.clone());
                        let default_action = if state.auto_open_browser_global {
                            devc_config::AutoForwardAction::OpenBrowserOnce
                        } else {
                            devc_config::AutoForwardAction::Silent
                        };
                        let action = matching_config
                            .map(|pfc| &pfc.action)
                            .unwrap_or(&default_action);
                        match action {
                            devc_config::AutoForwardAction::OpenBrowser => {
                                let protocol =
                                    matching_config.and_then(|pfc| pfc.protocol.as_deref());
                                let _ = crate::tunnel::open_in_browser(detected.port, protocol);
                            }
                            devc_config::AutoForwardAction::OpenBrowserOnce
                                if !state.opened_ports.contains(&key) =>
                            {
                                state.opened_ports.insert(key);
                                let protocol =
                                    matching_config.and_then(|pfc| pfc.protocol.as_deref());
                                let _ = crate::tunnel::open_in_browser(detected.port, protocol);
                            }
                            _ => {}
                        }
                    }
                    Err(e) => {
                        tracing::debug!("Failed to auto-forward port {}: {}", detected.port, e);
                    }
                }
            }
        }
    }
}

/// Spawn a background auto-forwarding task for use during shell sessions.
/// Returns a handle that yields the state back when the stop signal is sent.
pub fn spawn_shell_auto_forwarder(
    mut state: ShellAutoForwardState,
    mut stop_rx: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<ShellAutoForwardState> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    poll_detectors_background(&mut state).await;
                }
            }
        }
        state
    })
}
