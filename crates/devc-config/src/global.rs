//! Global configuration for devc
//!
//! Located at `~/.config/devc/config.toml`

use crate::{ConfigError, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Global devc configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GlobalConfig {
    pub defaults: DefaultsConfig,
    pub providers: ProvidersConfig,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            defaults: DefaultsConfig::default(),
            providers: ProvidersConfig::default(),
        }
    }
}

/// Default settings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DefaultsConfig {
    /// Default container provider ("docker" or "podman")
    pub provider: String,
    /// URL to dotfiles repository
    pub dotfiles_repo: Option<String>,
    /// Local path to dotfiles directory
    pub dotfiles_local: Option<String>,
    /// Default shell in containers
    pub shell: String,
    /// Default user in containers
    pub user: Option<String>,
    /// Enable SSH over stdio for proper TTY/resize support (default: true)
    pub ssh_enabled: Option<bool>,
    /// Path to SSH private key for container authentication
    pub ssh_key_path: Option<String>,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            provider: "podman".to_string(),
            dotfiles_repo: None,
            dotfiles_local: None,
            shell: "/bin/bash".to_string(),
            user: None,
            ssh_enabled: Some(true),
            ssh_key_path: None,
        }
    }
}

/// Provider-specific configurations
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProvidersConfig {
    pub docker: DockerConfig,
    pub podman: PodmanConfig,
}

/// Docker-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DockerConfig {
    /// Docker socket path
    pub socket: String,
    /// Additional Docker options
    #[serde(flatten)]
    pub extra: HashMap<String, toml::Value>,
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            socket: default_docker_socket(),
            extra: HashMap::new(),
        }
    }
}

#[cfg(windows)]
fn default_docker_socket() -> String {
    "//./pipe/docker_engine".to_string()
}

#[cfg(not(windows))]
fn default_docker_socket() -> String {
    "/var/run/docker.sock".to_string()
}

/// Podman-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PodmanConfig {
    /// Podman socket path
    pub socket: String,
    /// Additional Podman options
    #[serde(flatten)]
    pub extra: HashMap<String, toml::Value>,
}

impl Default for PodmanConfig {
    fn default() -> Self {
        Self {
            socket: default_podman_socket(),
            extra: HashMap::new(),
        }
    }
}

#[cfg(target_os = "linux")]
fn default_podman_socket() -> String {
    std::env::var("XDG_RUNTIME_DIR")
        .map(|dir| format!("{}/podman/podman.sock", dir))
        .unwrap_or_else(|_| "/run/user/1000/podman/podman.sock".to_string())
}

#[cfg(target_os = "macos")]
fn default_podman_socket() -> String {
    dirs::home_dir()
        .map(|h| {
            format!(
                "{}/.local/share/containers/podman/machine/podman-machine-default/podman.sock",
                h.display()
            )
        })
        .unwrap_or_else(|| "/var/run/podman.sock".to_string())
}

#[cfg(windows)]
fn default_podman_socket() -> String {
    "//./pipe/podman-machine-default".to_string()
}

impl GlobalConfig {
    /// Load global configuration from the default path
    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        Self::load_from(&path)
    }

    /// Load global configuration from a specific path
    pub fn load_from(path: &PathBuf) -> Result<Self> {
        if !path.exists() {
            tracing::debug!("Config file not found at {:?}, using defaults", path);
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path).map_err(|e| ConfigError::ReadError {
            path: path.clone(),
            source: e,
        })?;

        toml::from_str(&content).map_err(|e| ConfigError::TomlParseError {
            path: path.clone(),
            source: e,
        })
    }

    /// Save configuration to the default path
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        self.save_to(&path)
    }

    /// Save configuration to a specific path
    pub fn save_to(&self, path: &PathBuf) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ConfigError::WriteError {
                path: path.clone(),
                source: e,
            })?;
        }

        let content =
            toml::to_string_pretty(self).map_err(|e| ConfigError::Invalid(e.to_string()))?;

        std::fs::write(path, content).map_err(|e| ConfigError::WriteError {
            path: path.clone(),
            source: e,
        })
    }

    /// Get the default config file path
    pub fn config_path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("", "", "devc").ok_or(ConfigError::NoConfigDir)?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    /// Get the data directory path
    pub fn data_dir() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("", "", "devc").ok_or(ConfigError::NoDataDir)?;
        Ok(dirs.data_dir().to_path_buf())
    }

    /// Get the cache directory path
    pub fn cache_dir() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("", "", "devc").ok_or(ConfigError::NoDataDir)?;
        Ok(dirs.cache_dir().to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = GlobalConfig::default();
        assert_eq!(config.defaults.provider, "podman");
        assert_eq!(config.defaults.shell, "/bin/bash");
    }

    #[test]
    fn test_parse_config() {
        let toml = r#"
[defaults]
provider = "podman"
dotfiles_repo = "https://github.com/user/dotfiles"
shell = "/bin/zsh"

[providers.docker]
socket = "/var/run/docker.sock"

[providers.podman]
socket = "/run/user/1000/podman/podman.sock"
"#;

        let config: GlobalConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.defaults.provider, "podman");
        assert_eq!(
            config.defaults.dotfiles_repo,
            Some("https://github.com/user/dotfiles".to_string())
        );
        assert_eq!(config.defaults.shell, "/bin/zsh");
    }
}
