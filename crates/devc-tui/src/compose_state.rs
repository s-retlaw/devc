//! Compose service view state extracted from App

use ratatui::widgets::TableState;
use std::collections::HashMap;

/// State for the compose services detail view.
#[derive(Debug)]
pub struct ComposeViewState {
    /// Cached compose service info keyed by devc container ID
    pub services: HashMap<String, Vec<devc_provider::ComposeServiceInfo>>,
    /// Table state for compose services in detail view
    pub services_table_state: TableState,
    /// Currently selected service index in compose services table
    pub selected_service: usize,
    /// Whether compose services are currently being loaded
    pub services_loading: bool,
    /// Name of the service whose logs are being viewed (None = primary container)
    pub logs_service_name: Option<String>,
}

impl ComposeViewState {
    pub fn new() -> Self {
        Self {
            services: HashMap::new(),
            services_table_state: TableState::default(),
            selected_service: 0,
            services_loading: false,
            logs_service_name: None,
        }
    }
}

impl ComposeViewState {
    pub fn reset_detail(&mut self) {
        self.selected_service = 0;
        self.services_table_state = TableState::default();
        self.services_loading = false;
    }

    pub fn reset_logs(&mut self) {
        self.logs_service_name = None;
    }
}

impl Default for ComposeViewState {
    fn default() -> Self {
        Self::new()
    }
}
