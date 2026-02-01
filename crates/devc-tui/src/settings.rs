//! Settings form state for the TUI
//!
//! Global settings organized into logical sections.
//! Provider-specific settings are handled in the Providers tab.

use devc_config::GlobalConfig;

/// Settings section for visual grouping
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    ContainerDefaults,
    Dotfiles,
    Ssh,
}

impl SettingsSection {
    pub fn all() -> &'static [SettingsSection] {
        &[
            SettingsSection::ContainerDefaults,
            SettingsSection::Dotfiles,
            SettingsSection::Ssh,
        ]
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::ContainerDefaults => "CONTAINER DEFAULTS",
            Self::Dotfiles => "DOTFILES",
            Self::Ssh => "SSH / CONNECTION",
        }
    }

    pub fn fields(&self) -> &'static [SettingsField] {
        match self {
            Self::ContainerDefaults => &[SettingsField::DefaultShell, SettingsField::DefaultUser],
            Self::Dotfiles => &[SettingsField::DotfilesRepo, SettingsField::DotfilesLocal],
            Self::Ssh => &[SettingsField::SshEnabled, SettingsField::SshKeyPath],
        }
    }
}

/// Field in the settings form (global settings only)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
        }
    }

    pub fn section(&self) -> SettingsSection {
        match self {
            Self::DefaultShell | Self::DefaultUser => SettingsSection::ContainerDefaults,
            Self::DotfilesRepo | Self::DotfilesLocal => SettingsSection::Dotfiles,
            Self::SshEnabled | Self::SshKeyPath => SettingsSection::Ssh,
        }
    }

    pub fn is_editable(&self) -> bool {
        !matches!(self, Self::SshEnabled)
    }

    pub fn is_toggle(&self) -> bool {
        matches!(self, Self::SshEnabled)
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::DefaultShell => "Shell to use inside containers",
            Self::DefaultUser => "User to run as inside containers",
            Self::DotfilesRepo => "Git repository URL for dotfiles",
            Self::DotfilesLocal => "Local directory path for dotfiles",
            Self::SshEnabled => "Enable SSH for better TTY support",
            Self::SshKeyPath => "Path to SSH private key",
        }
    }
}

/// State for the settings view
pub struct SettingsState {
    /// Currently focused field index
    pub focused: usize,
    /// Whether we're in edit mode for the current field
    pub editing: bool,
    /// Edit buffer for text fields
    pub edit_buffer: String,
    /// Cursor position in edit buffer
    pub cursor: usize,
    /// Pending changes (not yet saved)
    pub draft: SettingsDraft,
    /// Whether changes have been made
    pub dirty: bool,
}

/// Draft settings that haven't been saved yet
#[derive(Clone)]
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
}

impl SettingsState {
    pub fn new(config: &GlobalConfig) -> Self {
        Self {
            focused: 0,
            editing: false,
            edit_buffer: String::new(),
            cursor: 0,
            draft: SettingsDraft::from_config(config),
            dirty: false,
        }
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

    pub fn start_edit(&mut self) {
        let field = self.focused_field();
        if field.is_editable() {
            self.editing = true;
            self.edit_buffer = self.draft.get_value(&field);
            self.cursor = self.edit_buffer.len();
        } else if field.is_toggle() {
            // Toggle immediately
            self.toggle_field();
        }
    }

    pub fn toggle_field(&mut self) {
        let field = self.focused_field();
        if let SettingsField::SshEnabled = field {
            self.draft.ssh_enabled = !self.draft.ssh_enabled;
            self.dirty = true;
        }
    }

    pub fn cancel_edit(&mut self) {
        self.editing = false;
        self.edit_buffer.clear();
        self.cursor = 0;
    }

    pub fn confirm_edit(&mut self) {
        if self.editing {
            let field = self.focused_field();
            self.draft.set_value(&field, &self.edit_buffer);
            self.dirty = true;
            self.editing = false;
            self.edit_buffer.clear();
            self.cursor = 0;
        }
    }

    pub fn insert_char(&mut self, c: char) {
        if self.editing {
            self.edit_buffer.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    pub fn delete_char(&mut self) {
        if self.editing && self.cursor > 0 {
            self.cursor -= 1;
            self.edit_buffer.remove(self.cursor);
        }
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.cursor < self.edit_buffer.len() {
            self.cursor += 1;
        }
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
    }

    /// Reset draft from config
    pub fn reset_from_config(&mut self, config: &GlobalConfig) {
        self.draft = SettingsDraft::from_config(config);
        self.dirty = false;
        self.focused = 0;
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
        }
    }

    pub fn set_value(&mut self, field: &SettingsField, value: &str) {
        let value_opt = if value.is_empty() { None } else { Some(value.to_string()) };

        match field {
            SettingsField::DefaultShell => self.shell = value.to_string(),
            SettingsField::DefaultUser => self.user = value_opt,
            SettingsField::DotfilesRepo => self.dotfiles_repo = value_opt,
            SettingsField::DotfilesLocal => self.dotfiles_local = value_opt,
            SettingsField::SshEnabled => {
                self.ssh_enabled = value == "true" || value == "1" || value == "yes";
            }
            SettingsField::SshKeyPath => self.ssh_key_path = value_opt,
        }
    }
}

/// State for the provider detail view
pub struct ProviderDetailState {
    /// Which field is focused (0 = socket)
    pub focused: usize,
    /// Whether we're editing
    pub editing: bool,
    /// Edit buffer
    pub edit_buffer: String,
    /// Cursor position
    pub cursor: usize,
    /// Whether changes have been made
    pub dirty: bool,
    /// Connection test result (None = not tested, Some(true) = connected, Some(false) = failed)
    pub connection_status: Option<bool>,
    /// Connection error message if failed
    pub connection_error: Option<String>,
}

impl ProviderDetailState {
    pub fn new() -> Self {
        Self {
            focused: 0,
            editing: false,
            edit_buffer: String::new(),
            cursor: 0,
            dirty: false,
            connection_status: None,
            connection_error: None,
        }
    }

    pub fn start_edit(&mut self, current_value: &str) {
        self.editing = true;
        self.edit_buffer = current_value.to_string();
        self.cursor = self.edit_buffer.len();
    }

    pub fn cancel_edit(&mut self) {
        self.editing = false;
        self.edit_buffer.clear();
        self.cursor = 0;
    }

    pub fn confirm_edit(&mut self) -> Option<String> {
        if self.editing {
            self.editing = false;
            self.dirty = true;
            let value = self.edit_buffer.clone();
            self.edit_buffer.clear();
            self.cursor = 0;
            Some(value)
        } else {
            None
        }
    }

    pub fn insert_char(&mut self, c: char) {
        if self.editing {
            self.edit_buffer.insert(self.cursor, c);
            self.cursor += 1;
        }
    }

    pub fn delete_char(&mut self) {
        if self.editing && self.cursor > 0 {
            self.cursor -= 1;
            self.edit_buffer.remove(self.cursor);
        }
    }

    pub fn move_cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
    }

    pub fn move_cursor_right(&mut self) {
        if self.cursor < self.edit_buffer.len() {
            self.cursor += 1;
        }
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
