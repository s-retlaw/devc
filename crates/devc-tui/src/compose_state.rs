//! Compose service view state extracted from App

use ratatui::widgets::TableState;
use std::collections::HashMap;

/// State for the compose services detail view.
pub struct ComposeViewState {
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
}

impl ComposeViewState {
    pub fn new() -> Self {
        Self {
            compose_services: HashMap::new(),
            compose_services_table_state: TableState::default(),
            compose_selected_service: 0,
            compose_services_loading: false,
            logs_service_name: None,
        }
    }
}

impl Default for ComposeViewState {
    fn default() -> Self {
        Self::new()
    }
}
