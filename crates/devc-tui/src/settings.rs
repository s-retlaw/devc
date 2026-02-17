//! Settings form state for the TUI
//!
//! Global settings organized into logical sections.
//! Provider-specific settings are handled in the Providers tab.

use crate::widgets::TextInputState;
use devc_config::GlobalConfig;
use devc_core::agents::{AgentKind, HostAgentAvailability};
use std::collections::HashMap;

/// Settings section for visual grouping
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    ContainerDefaults,
    Dotfiles,
    Ssh,
    Credentials,
    Agents,
}

impl SettingsSection {
    pub fn all() -> &'static [SettingsSection] {
        &[
            SettingsSection::ContainerDefaults,
            SettingsSection::Dotfiles,
            SettingsSection::Ssh,
            SettingsSection::Credentials,
            SettingsSection::Agents,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::ContainerDefaults => "CONTAINER DEFAULTS",
            Self::Dotfiles => "DOTFILES",
            Self::Ssh => "SSH / CONNECTION",
            Self::Credentials => "CREDENTIALS",
            Self::Agents => "AGENTS",
        }
    }

    pub fn fields(&self) -> &'static [SettingsField] {
        match self {
            Self::ContainerDefaults => &[SettingsField::DefaultShell, SettingsField::DefaultUser],
            Self::Dotfiles => &[SettingsField::DotfilesRepo, SettingsField::DotfilesLocal],
            Self::Ssh => &[SettingsField::SshEnabled, SettingsField::SshKeyPath],
            Self::Credentials => &[
                SettingsField::CredentialsDocker,
                SettingsField::CredentialsGit,
            ],
            Self::Agents => &[
                SettingsField::AgentCodexEnabled,
                SettingsField::AgentClaudeEnabled,
                SettingsField::AgentCursorEnabled,
                SettingsField::AgentGeminiEnabled,
            ],
        }
    }
}

/// Field in the settings form (global settings only)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsField {
    // Container Defaults
    DefaultShell,
    DefaultUser,
    // Dotfiles
    DotfilesRepo,
    DotfilesLocal,
    // SSH
    SshEnabled,
    SshKeyPath,
    // Credentials
    CredentialsDocker,
    CredentialsGit,
    // Agents
    AgentCodexEnabled,
    AgentClaudeEnabled,
    AgentCursorEnabled,
    AgentGeminiEnabled,
}

impl SettingsField {
    pub fn all() -> &'static [SettingsField] {
        &[
            // Container Defaults
            SettingsField::DefaultShell,
            SettingsField::DefaultUser,
            // Dotfiles
            SettingsField::DotfilesRepo,
            SettingsField::DotfilesLocal,
            // SSH
            SettingsField::SshEnabled,
            SettingsField::SshKeyPath,
            // Credentials
            SettingsField::CredentialsDocker,
            SettingsField::CredentialsGit,
            // Agents
            SettingsField::AgentCodexEnabled,
            SettingsField::AgentClaudeEnabled,
            SettingsField::AgentCursorEnabled,
            SettingsField::AgentGeminiEnabled,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::DefaultShell => "Default Shell",
            Self::DefaultUser => "Default User",
            Self::DotfilesRepo => "Repository URL",
            Self::DotfilesLocal => "Local Path",
            Self::SshEnabled => "SSH Enabled",
            Self::SshKeyPath => "SSH Key Path",
            Self::CredentialsDocker => "Docker Credentials",
            Self::CredentialsGit => "Git Credentials",
            Self::AgentCodexEnabled => "Codex",
            Self::AgentClaudeEnabled => "Claude",
            Self::AgentCursorEnabled => "Cursor",
            Self::AgentGeminiEnabled => "Gemini",
        }
    }

    pub fn section(&self) -> SettingsSection {
        match self {
            Self::DefaultShell | Self::DefaultUser => SettingsSection::ContainerDefaults,
            Self::DotfilesRepo | Self::DotfilesLocal => SettingsSection::Dotfiles,
            Self::SshEnabled | Self::SshKeyPath => SettingsSection::Ssh,
            Self::CredentialsDocker | Self::CredentialsGit => SettingsSection::Credentials,
            Self::AgentCodexEnabled
            | Self::AgentClaudeEnabled
            | Self::AgentCursorEnabled
            | Self::AgentGeminiEnabled => SettingsSection::Agents,
        }
    }

    pub fn is_editable(&self) -> bool {
        !matches!(
            self,
            Self::SshEnabled
                | Self::CredentialsDocker
                | Self::CredentialsGit
                | Self::AgentCodexEnabled
                | Self::AgentClaudeEnabled
                | Self::AgentCursorEnabled
                | Self::AgentGeminiEnabled
        )
    }

    pub fn is_toggle(&self) -> bool {
        matches!(
            self,
            Self::SshEnabled
                | Self::CredentialsDocker
                | Self::CredentialsGit
                | Self::AgentCodexEnabled
                | Self::AgentClaudeEnabled
                | Self::AgentCursorEnabled
                | Self::AgentGeminiEnabled
        )
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::DefaultShell => "Shell to use inside containers",
            Self::DefaultUser => "User to run as inside containers",
            Self::DotfilesRepo => "Git repository URL for dotfiles",
            Self::DotfilesLocal => "Local directory path for dotfiles",
            Self::SshEnabled => "Enable SSH for better TTY support",
            Self::SshKeyPath => "Path to SSH private key",
            Self::CredentialsDocker => "Forward Docker registry credentials into containers",
            Self::CredentialsGit => "Forward Git credentials into containers",
            Self::AgentCodexEnabled => {
                "Enable Codex config/auth sync and install-if-missing (requires Node/npm)"
            }
            Self::AgentClaudeEnabled => {
                "Enable Claude config/auth sync and install-if-missing (requires Node/npm)"
            }
            Self::AgentCursorEnabled => {
                "Enable Cursor config/auth sync and install-if-missing (requires Node/npm)"
            }
            Self::AgentGeminiEnabled => {
                "Enable Gemini config/auth sync and install-if-missing (requires Node/npm)"
            }
        }
    }

    pub fn is_agent_field(&self) -> bool {
        matches!(
            self,
            Self::AgentCodexEnabled
                | Self::AgentClaudeEnabled
                | Self::AgentCursorEnabled
                | Self::AgentGeminiEnabled
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct AgentAvailability {
    pub available: bool,
    pub reason: Option<String>,
}

/// State for the settings view
pub struct SettingsState {
    /// Currently focused field index
    pub focused: usize,
    /// Whether we're in edit mode for the current field
    pub editing: bool,
    /// Text input state for editing
    input: TextInputState,
    /// Pending changes (not yet saved)
    pub draft: SettingsDraft,
    /// Snapshot of last-saved state for dirty detection
    pub saved: SettingsDraft,
    /// Host availability for agent toggles.
    pub agent_availability: HashMap<SettingsField, AgentAvailability>,
}

// Legacy accessor methods for backwards compatibility with existing code
impl SettingsState {
    /// Get the edit buffer (for display)
    pub fn edit_buffer(&self) -> &str {
        self.input.value()
    }

    /// Get the cursor position (for display)
    pub fn cursor(&self) -> usize {
        self.input.cursor()
    }
}

/// Draft settings that haven't been saved yet
#[derive(Clone, PartialEq)]
pub struct SettingsDraft {
    // Container defaults
    pub shell: String,
    pub user: Option<String>,
    // Dotfiles
    pub dotfiles_repo: Option<String>,
    pub dotfiles_local: Option<String>,
    // SSH
    pub ssh_enabled: bool,
    pub ssh_key_path: Option<String>,
    // Credentials
    pub credentials_docker: bool,
    pub credentials_git: bool,
    // Agents
    pub agent_codex_enabled: bool,
    pub agent_claude_enabled: bool,
    pub agent_cursor_enabled: bool,
    pub agent_gemini_enabled: bool,
}

impl SettingsState {
    pub fn new(config: &GlobalConfig) -> Self {
        let draft = SettingsDraft::from_config(config);
        Self {
            focused: 0,
            editing: false,
            input: TextInputState::new(),
            saved: draft.clone(),
            draft,
            agent_availability: HashMap::new(),
        }
    }

    /// Whether draft differs from last-saved state
    pub fn dirty(&self) -> bool {
        self.draft != self.saved
    }

    pub fn focused_field(&self) -> SettingsField {
        SettingsField::all()[self.focused]
    }

    pub fn move_up(&mut self) {
        if self.focused > 0 {
            self.focused -= 1;
        }
    }

    pub fn move_down(&mut self) {
        let fields = SettingsField::all();
        if self.focused < fields.len() - 1 {
            self.focused += 1;
        }
    }

    pub fn start_edit(&mut self) -> Option<String> {
        let field = self.focused_field();
        if field.is_editable() {
            self.editing = true;
            self.input.set_value(&self.draft.get_value(&field));
            None
        } else if field.is_toggle() {
            // Toggle immediately
            self.toggle_field()
        } else {
            None
        }
    }

    pub fn toggle_field(&mut self) -> Option<String> {
        let field = self.focused_field();
        if self.field_disabled(field) {
            return Some(self.unavailable_message(field));
        }
        match field {
            SettingsField::SshEnabled => {
                self.draft.ssh_enabled = !self.draft.ssh_enabled;
            }
            SettingsField::CredentialsDocker => {
                self.draft.credentials_docker = !self.draft.credentials_docker;
            }
            SettingsField::CredentialsGit => {
                self.draft.credentials_git = !self.draft.credentials_git;
            }
            SettingsField::AgentCodexEnabled => {
                self.draft.agent_codex_enabled = !self.draft.agent_codex_enabled;
            }
            SettingsField::AgentClaudeEnabled => {
                self.draft.agent_claude_enabled = !self.draft.agent_claude_enabled;
            }
            SettingsField::AgentCursorEnabled => {
                self.draft.agent_cursor_enabled = !self.draft.agent_cursor_enabled;
            }
            SettingsField::AgentGeminiEnabled => {
                self.draft.agent_gemini_enabled = !self.draft.agent_gemini_enabled;
            }
            _ => {}
        }
        None
    }

    pub fn cancel_edit(&mut self) {
        self.editing = false;
        self.input.clear();
    }

    pub fn confirm_edit(&mut self) {
        if self.editing {
            let field = self.focused_field();
            self.draft.set_value(&field, self.input.value());
            self.editing = false;
            self.input.clear();
        }
    }

    pub fn insert_char(&mut self, c: char) {
        if self.editing {
            self.input.insert(c);
        }
    }

    pub fn delete_char(&mut self) {
        if self.editing {
            self.input.backspace();
        }
    }

    pub fn move_cursor_left(&mut self) {
        self.input.move_left();
    }

    pub fn move_cursor_right(&mut self) {
        self.input.move_right();
    }

    /// Apply draft to a config
    pub fn apply_to_config(&self, config: &mut GlobalConfig) {
        // Container defaults
        config.defaults.shell = self.draft.shell.clone();
        config.defaults.user = self.draft.user.clone();
        // Dotfiles
        config.defaults.dotfiles_repo = self.draft.dotfiles_repo.clone();
        config.defaults.dotfiles_local = self.draft.dotfiles_local.clone();
        // SSH
        config.defaults.ssh_enabled = Some(self.draft.ssh_enabled);
        config.defaults.ssh_key_path = self.draft.ssh_key_path.clone();
        // Credentials
        config.credentials.docker = self.draft.credentials_docker;
        config.credentials.git = self.draft.credentials_git;
        // Agents
        config.agents.codex.enabled = Some(
            self.draft.agent_codex_enabled
                && self.agent_field_available(SettingsField::AgentCodexEnabled),
        );
        config.agents.claude.enabled = Some(
            self.draft.agent_claude_enabled
                && self.agent_field_available(SettingsField::AgentClaudeEnabled),
        );
        config.agents.cursor.enabled = Some(
            self.draft.agent_cursor_enabled
                && self.agent_field_available(SettingsField::AgentCursorEnabled),
        );
        config.agents.gemini.enabled = Some(
            self.draft.agent_gemini_enabled
                && self.agent_field_available(SettingsField::AgentGeminiEnabled),
        );
    }

    /// Reset draft from config
    pub fn reset_from_config(&mut self, config: &GlobalConfig) {
        self.draft = SettingsDraft::from_config(config);
        self.saved = self.draft.clone();
        self.focused = 0;
    }

    pub fn apply_agent_host_availability(
        &mut self,
        availability: &[HostAgentAvailability],
        config: &GlobalConfig,
    ) {
        self.agent_availability.clear();
        for item in availability {
            let Some(field) = Self::agent_field_for_kind(item.agent) else {
                continue;
            };
            self.agent_availability.insert(
                field,
                AgentAvailability {
                    available: item.available,
                    reason: item.reason.clone(),
                },
            );
            match Self::agent_enabled_override(config, item.agent) {
                Some(explicit) => {
                    self.draft
                        .set_agent_enabled(field, explicit && item.available);
                    self.saved
                        .set_agent_enabled(field, explicit && item.available);
                }
                None => {
                    self.draft.set_agent_enabled(field, item.available);
                    self.saved.set_agent_enabled(field, item.available);
                }
            }
        }
    }

    pub fn field_disabled(&self, field: SettingsField) -> bool {
        field.is_agent_field() && !self.agent_field_available(field)
    }

    pub fn field_unavailable_reason(&self, field: SettingsField) -> Option<&str> {
        self.agent_availability.get(&field).and_then(|a| {
            if a.available {
                None
            } else {
                a.reason.as_deref()
            }
        })
    }

    fn agent_field_available(&self, field: SettingsField) -> bool {
        self.agent_availability
            .get(&field)
            .map(|a| a.available)
            .unwrap_or(true)
    }

    fn unavailable_message(&self, field: SettingsField) -> String {
        let reason = self
            .field_unavailable_reason(field)
            .unwrap_or("host config unavailable");
        format!(
            "{} not installed on host to inject ({})",
            field.label(),
            reason
        )
    }

    fn agent_field_for_kind(kind: AgentKind) -> Option<SettingsField> {
        match kind {
            AgentKind::Codex => Some(SettingsField::AgentCodexEnabled),
            AgentKind::Claude => Some(SettingsField::AgentClaudeEnabled),
            AgentKind::Cursor => Some(SettingsField::AgentCursorEnabled),
            AgentKind::Gemini => Some(SettingsField::AgentGeminiEnabled),
        }
    }

    fn agent_enabled_override(config: &GlobalConfig, kind: AgentKind) -> Option<bool> {
        match kind {
            AgentKind::Codex => config.agents.codex.enabled,
            AgentKind::Claude => config.agents.claude.enabled,
            AgentKind::Cursor => config.agents.cursor.enabled,
            AgentKind::Gemini => config.agents.gemini.enabled,
        }
    }
}

impl SettingsDraft {
    pub fn from_config(config: &GlobalConfig) -> Self {
        Self {
            shell: config.defaults.shell.clone(),
            user: config.defaults.user.clone(),
            dotfiles_repo: config.defaults.dotfiles_repo.clone(),
            dotfiles_local: config.defaults.dotfiles_local.clone(),
            ssh_enabled: config.defaults.ssh_enabled.unwrap_or(true),
            ssh_key_path: config.defaults.ssh_key_path.clone(),
            credentials_docker: config.credentials.docker,
            credentials_git: config.credentials.git,
            agent_codex_enabled: config.agents.codex.enabled.unwrap_or(false),
            agent_claude_enabled: config.agents.claude.enabled.unwrap_or(false),
            agent_cursor_enabled: config.agents.cursor.enabled.unwrap_or(false),
            agent_gemini_enabled: config.agents.gemini.enabled.unwrap_or(false),
        }
    }

    pub fn get_value(&self, field: &SettingsField) -> String {
        match field {
            SettingsField::DefaultShell => self.shell.clone(),
            SettingsField::DefaultUser => self.user.clone().unwrap_or_default(),
            SettingsField::DotfilesRepo => self.dotfiles_repo.clone().unwrap_or_default(),
            SettingsField::DotfilesLocal => self.dotfiles_local.clone().unwrap_or_default(),
            SettingsField::SshEnabled => {
                if self.ssh_enabled { "true" } else { "false" }.to_string()
            }
            SettingsField::SshKeyPath => self.ssh_key_path.clone().unwrap_or_default(),
            SettingsField::CredentialsDocker => if self.credentials_docker {
                "true"
            } else {
                "false"
            }
            .to_string(),
            SettingsField::CredentialsGit => if self.credentials_git {
                "true"
            } else {
                "false"
            }
            .to_string(),
            SettingsField::AgentCodexEnabled => if self.agent_codex_enabled {
                "true"
            } else {
                "false"
            }
            .to_string(),
            SettingsField::AgentClaudeEnabled => if self.agent_claude_enabled {
                "true"
            } else {
                "false"
            }
            .to_string(),
            SettingsField::AgentCursorEnabled => if self.agent_cursor_enabled {
                "true"
            } else {
                "false"
            }
            .to_string(),
            SettingsField::AgentGeminiEnabled => if self.agent_gemini_enabled {
                "true"
            } else {
                "false"
            }
            .to_string(),
        }
    }

    pub fn set_value(&mut self, field: &SettingsField, value: &str) {
        let value_opt = if value.is_empty() {
            None
        } else {
            Some(value.to_string())
        };

        match field {
            SettingsField::DefaultShell => self.shell = value.to_string(),
            SettingsField::DefaultUser => self.user = value_opt,
            SettingsField::DotfilesRepo => self.dotfiles_repo = value_opt,
            SettingsField::DotfilesLocal => self.dotfiles_local = value_opt,
            SettingsField::SshEnabled => {
                self.ssh_enabled = value == "true" || value == "1" || value == "yes";
            }
            SettingsField::SshKeyPath => self.ssh_key_path = value_opt,
            SettingsField::CredentialsDocker => {
                self.credentials_docker = value == "true" || value == "1" || value == "yes";
            }
            SettingsField::CredentialsGit => {
                self.credentials_git = value == "true" || value == "1" || value == "yes";
            }
            SettingsField::AgentCodexEnabled => {
                self.agent_codex_enabled = value == "true" || value == "1" || value == "yes";
            }
            SettingsField::AgentClaudeEnabled => {
                self.agent_claude_enabled = value == "true" || value == "1" || value == "yes";
            }
            SettingsField::AgentCursorEnabled => {
                self.agent_cursor_enabled = value == "true" || value == "1" || value == "yes";
            }
            SettingsField::AgentGeminiEnabled => {
                self.agent_gemini_enabled = value == "true" || value == "1" || value == "yes";
            }
        }
    }

    fn set_agent_enabled(&mut self, field: SettingsField, enabled: bool) {
        match field {
            SettingsField::AgentCodexEnabled => self.agent_codex_enabled = enabled,
            SettingsField::AgentClaudeEnabled => self.agent_claude_enabled = enabled,
            SettingsField::AgentCursorEnabled => self.agent_cursor_enabled = enabled,
            SettingsField::AgentGeminiEnabled => self.agent_gemini_enabled = enabled,
            _ => {}
        }
    }
}

/// State for the provider detail view
pub struct ProviderDetailState {
    /// Which field is focused (0 = socket)
    pub focused: usize,
    /// Whether we're editing
    pub editing: bool,
    /// Text input state for editing
    input: TextInputState,
    /// Whether changes have been made
    pub dirty: bool,
    /// Connection test result (None = not tested, Some(true) = connected, Some(false) = failed)
    pub connection_status: Option<bool>,
    /// Connection error message if failed
    pub connection_error: Option<String>,
}

// Legacy accessor methods for backwards compatibility with existing code
impl ProviderDetailState {
    /// Get the edit buffer (for display)
    pub fn edit_buffer(&self) -> &str {
        self.input.value()
    }

    /// Get the cursor position (for display)
    pub fn cursor(&self) -> usize {
        self.input.cursor()
    }
}

impl ProviderDetailState {
    pub fn new() -> Self {
        Self {
            focused: 0,
            editing: false,
            input: TextInputState::new(),
            dirty: false,
            connection_status: None,
            connection_error: None,
        }
    }

    pub fn start_edit(&mut self, current_value: &str) {
        self.editing = true;
        self.input.set_value(current_value);
    }

    pub fn cancel_edit(&mut self) {
        self.editing = false;
        self.input.clear();
    }

    pub fn confirm_edit(&mut self) -> Option<String> {
        if self.editing {
            self.editing = false;
            self.dirty = true;
            let value = self.input.value().to_string();
            self.input.clear();
            Some(value)
        } else {
            None
        }
    }

    pub fn insert_char(&mut self, c: char) {
        if self.editing {
            self.input.insert(c);
        }
    }

    pub fn delete_char(&mut self) {
        if self.editing {
            self.input.backspace();
        }
    }

    pub fn move_cursor_left(&mut self) {
        self.input.move_left();
    }

    pub fn move_cursor_right(&mut self) {
        self.input.move_right();
    }

    pub fn set_connection_result(&mut self, connected: bool, error: Option<String>) {
        self.connection_status = Some(connected);
        self.connection_error = if connected { None } else { error };
    }

    pub fn clear_connection_status(&mut self) {
        self.connection_status = None;
        self.connection_error = None;
    }
}

impl Default for ProviderDetailState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devc_config::GlobalConfig;

    // ==================== SettingsState TextInput Tests ====================
    // Note: Low-level cursor/insert/delete behavior is tested in widgets::text_input
    // These tests focus on the SettingsState high-level behavior

    #[test]
    fn test_start_edit_sets_cursor_at_end() {
        let config = GlobalConfig::default();
        let mut state = SettingsState::new(&config);

        state.draft.shell = "/bin/zsh".to_string();
        state.focused = 0; // DefaultShell

        state.start_edit();
        assert!(state.editing);
        assert_eq!(state.edit_buffer(), "/bin/zsh");
        assert_eq!(state.cursor(), 8); // At end of string
    }

    #[test]
    fn test_cancel_edit_clears_buffer() {
        let config = GlobalConfig::default();
        let mut state = SettingsState::new(&config);

        state.draft.shell = "/bin/zsh".to_string();
        state.focused = 0;
        state.start_edit();
        state.insert_char('!');

        state.cancel_edit();
        assert!(!state.editing);
        assert!(state.edit_buffer().is_empty());
        assert_eq!(state.cursor(), 0);
    }

    #[test]
    fn test_confirm_edit_saves_to_draft() {
        let config = GlobalConfig::default();
        let mut state = SettingsState::new(&config);

        state.focused = 0; // DefaultShell
        state.start_edit();
        // Clear and type new value
        while !state.edit_buffer().is_empty() {
            state.delete_char();
        }
        for c in "/bin/fish".chars() {
            state.insert_char(c);
        }

        state.confirm_edit();
        assert!(!state.editing);
        assert!(state.dirty());
        assert_eq!(state.draft.shell, "/bin/fish");
    }

    #[test]
    fn test_insert_only_works_when_editing() {
        let config = GlobalConfig::default();
        let mut state = SettingsState::new(&config);

        // Not in edit mode
        state.insert_char('X');
        assert!(state.edit_buffer().is_empty());

        // In edit mode
        state.draft.shell = "hello".to_string();
        state.focused = 0;
        state.start_edit();
        state.insert_char('!');
        assert_eq!(state.edit_buffer(), "hello!");
    }

    // ==================== ProviderDetailState TextInput Tests ====================

    #[test]
    fn test_provider_start_edit() {
        let mut state = ProviderDetailState::new();

        state.start_edit("/var/run/docker.sock");
        assert!(state.editing);
        assert_eq!(state.cursor(), 20);
        assert_eq!(state.edit_buffer(), "/var/run/docker.sock");
    }

    #[test]
    fn test_provider_confirm_edit_returns_value() {
        let mut state = ProviderDetailState::new();

        state.start_edit("/var/run/docker.sock");
        // Clear and type new value
        while state.cursor() > 0 {
            state.delete_char();
        }
        for c in "/run/podman/podman.sock".chars() {
            state.insert_char(c);
        }

        let result = state.confirm_edit();
        assert!(result.is_some());
        assert_eq!(result.unwrap(), "/run/podman/podman.sock");
        assert!(!state.editing);
        assert!(state.dirty);
    }

    #[test]
    fn test_provider_cancel_edit() {
        let mut state = ProviderDetailState::new();

        state.start_edit("/var/run/docker.sock");
        state.insert_char('!');

        state.cancel_edit();
        assert!(!state.editing);
        assert!(state.edit_buffer().is_empty());
        assert_eq!(state.cursor(), 0);
    }

    // ==================== Navigation Tests ====================

    #[test]
    fn test_settings_navigation() {
        let config = GlobalConfig::default();
        let mut state = SettingsState::new(&config);

        assert_eq!(state.focused, 0);
        assert_eq!(state.focused_field(), SettingsField::DefaultShell);

        state.move_down();
        assert_eq!(state.focused_field(), SettingsField::DefaultUser);

        state.move_down();
        assert_eq!(state.focused_field(), SettingsField::DotfilesRepo);

        state.move_up();
        assert_eq!(state.focused_field(), SettingsField::DefaultUser);
    }

    #[test]
    fn test_settings_navigation_bounds() {
        let config = GlobalConfig::default();
        let mut state = SettingsState::new(&config);

        // At top, can't go up
        state.focused = 0;
        state.move_up();
        assert_eq!(state.focused, 0);

        // At bottom, can't go down
        state.focused = SettingsField::all().len() - 1;
        state.move_down();
        assert_eq!(state.focused, SettingsField::all().len() - 1);
    }

    #[test]
    fn test_toggle_field() {
        let config = GlobalConfig::default();
        let mut state = SettingsState::new(&config);

        // Go to SSH Enabled field
        state.focused = 4; // SshEnabled
        let initial = state.draft.ssh_enabled;

        assert!(state.toggle_field().is_none());
        assert_eq!(state.draft.ssh_enabled, !initial);
        assert!(state.dirty());
    }

    #[test]
    fn test_agent_toggle_applies_to_config() {
        let config = GlobalConfig::default();
        let mut state = SettingsState::new(&config);

        // Last field in list is AgentGeminiEnabled.
        state.focused = SettingsField::all().len() - 1;
        assert!(state.toggle_field().is_none());

        let mut updated = GlobalConfig::default();
        state.apply_to_config(&mut updated);
        assert_eq!(updated.agents.gemini.enabled, Some(true));
    }

    #[test]
    fn test_unavailable_agent_forced_disabled_and_blocked() {
        let mut config = GlobalConfig::default();
        config.agents.codex.enabled = Some(true);
        let mut state = SettingsState::new(&config);
        state.apply_agent_host_availability(
            &[HostAgentAvailability {
                agent: AgentKind::Codex,
                available: false,
                reason: Some("host config missing: /tmp/.codex".to_string()),
            }],
            &config,
        );

        assert!(!state.draft.agent_codex_enabled);
        state.focused = SettingsField::all()
            .iter()
            .position(|f| *f == SettingsField::AgentCodexEnabled)
            .unwrap();
        let msg = state.start_edit();
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("not installed on host to inject"));

        let mut updated = config.clone();
        state.apply_to_config(&mut updated);
        assert_eq!(updated.agents.codex.enabled, Some(false));
    }
}
