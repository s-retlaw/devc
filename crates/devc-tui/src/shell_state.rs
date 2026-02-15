//! Shell session state extracted from App

use crate::app::ShellSession;
use std::collections::HashMap;

/// State for persistent shell sessions.
pub struct ShellState {
    /// Persistent shell sessions keyed by container_id
    pub shell_sessions: HashMap<String, ShellSession>,
    /// Which container's shell is currently active (when View::Shell)
    pub active_shell_container: Option<String>,
}

impl ShellState {
    pub fn new() -> Self {
        Self {
            shell_sessions: HashMap::new(),
            active_shell_container: None,
        }
    }
}

impl Default for ShellState {
    fn default() -> Self {
        Self::new()
    }
}
