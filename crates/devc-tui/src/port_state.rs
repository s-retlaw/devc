//! Port forwarding state extracted from App

use crate::ports::{DetectedPort, PortDetectionUpdate};
use crate::tunnel::PortForwarder;
use ratatui::widgets::TableState;
use std::collections::{HashMap, HashSet};
use tokio::sync::mpsc;

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
        }
    }

    /// Handle a port detection update (updates detected_ports list)
    pub fn handle_port_update(&mut self, update: PortDetectionUpdate) {
        let forwarded_ports: HashSet<u16> = if let Some(ref container_id) = self.provider_container_id {
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
            self.selected_port = self.selected_port
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
}

impl Default for PortForwardingState {
    fn default() -> Self {
        Self::new()
    }
}
