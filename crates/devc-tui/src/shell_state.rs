//! Shell session state extracted from App

#[cfg(unix)]
use crate::shell::PtyShell;
use std::collections::HashMap;

/// Active shell session state (persistent across attach/detach cycles)
pub struct ShellSession {
    pub container_id: String,
    pub container_name: String,
    pub provider_container_id: String,
    pub runtime_program: String,
    pub runtime_prefix: Vec<String>,
    /// Effective user for the shell (from config override, metadata, or devcontainer.json)
    pub user: Option<String>,
    /// Working directory for the shell (from metadata or devcontainer.json workspaceFolder)
    pub working_dir: Option<String>,
    #[cfg(unix)]
    pub pty: Option<PtyShell>,
}

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
