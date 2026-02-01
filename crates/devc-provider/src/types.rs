//! Common types for container providers

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use tokio::io::AsyncRead;

/// Container ID wrapper
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContainerId(pub String);

impl ContainerId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn short(&self) -> &str {
        if self.0.len() > 12 {
            &self.0[..12]
        } else {
            &self.0
        }
    }
}

impl std::fmt::Display for ContainerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl AsRef<str> for ContainerId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Image ID wrapper
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ImageId(pub String);

impl ImageId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for ImageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Container provider type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    Docker,
    Podman,
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Docker => write!(f, "docker"),
            Self::Podman => write!(f, "podman"),
        }
    }
}

impl std::str::FromStr for ProviderType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "docker" => Ok(Self::Docker),
            "podman" => Ok(Self::Podman),
            _ => Err(format!("Unknown provider type: {}", s)),
        }
    }
}

/// Container status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContainerStatus {
    Created,
    Running,
    Paused,
    Restarting,
    Removing,
    Exited,
    Dead,
    Unknown,
}

impl std::fmt::Display for ContainerStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => write!(f, "created"),
            Self::Running => write!(f, "running"),
            Self::Paused => write!(f, "paused"),
            Self::Restarting => write!(f, "restarting"),
            Self::Removing => write!(f, "removing"),
            Self::Exited => write!(f, "exited"),
            Self::Dead => write!(f, "dead"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

impl From<&str> for ContainerStatus {
    fn from(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "created" => Self::Created,
            "running" => Self::Running,
            "paused" => Self::Paused,
            "restarting" => Self::Restarting,
            "removing" => Self::Removing,
            "exited" => Self::Exited,
            "dead" => Self::Dead,
            _ => Self::Unknown,
        }
    }
}

/// Build configuration for creating images
#[derive(Debug, Clone, Default)]
pub struct BuildConfig {
    /// Path to the build context
    pub context: PathBuf,
    /// Dockerfile path (relative to context)
    pub dockerfile: String,
    /// Image tag
    pub tag: String,
    /// Build arguments
    pub build_args: HashMap<String, String>,
    /// Target stage for multi-stage builds
    pub target: Option<String>,
    /// Cache from images
    pub cache_from: Vec<String>,
    /// Labels to apply
    pub labels: HashMap<String, String>,
    /// No cache
    pub no_cache: bool,
    /// Pull base image
    pub pull: bool,
}

/// Configuration for creating a container
#[derive(Debug, Clone, Default)]
pub struct CreateContainerConfig {
    /// Image to use
    pub image: String,
    /// Container name
    pub name: Option<String>,
    /// Command to run
    pub cmd: Option<Vec<String>>,
    /// Entrypoint override
    pub entrypoint: Option<Vec<String>>,
    /// Environment variables
    pub env: HashMap<String, String>,
    /// Working directory
    pub working_dir: Option<String>,
    /// User to run as
    pub user: Option<String>,
    /// Volume mounts
    pub mounts: Vec<MountConfig>,
    /// Port mappings
    pub ports: Vec<PortConfig>,
    /// Labels
    pub labels: HashMap<String, String>,
    /// Hostname
    pub hostname: Option<String>,
    /// Allocate TTY
    pub tty: bool,
    /// Keep STDIN open
    pub stdin_open: bool,
    /// Network mode
    pub network_mode: Option<String>,
    /// Privileged mode
    pub privileged: bool,
    /// Capabilities to add
    pub cap_add: Vec<String>,
    /// Capabilities to drop
    pub cap_drop: Vec<String>,
    /// Security options
    pub security_opt: Vec<String>,
}

/// Mount configuration
#[derive(Debug, Clone)]
pub struct MountConfig {
    /// Mount type (bind, volume, tmpfs)
    pub mount_type: MountType,
    /// Source path or volume name
    pub source: String,
    /// Target path in container
    pub target: String,
    /// Read-only
    pub read_only: bool,
}

/// Mount type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountType {
    Bind,
    Volume,
    Tmpfs,
}

impl std::fmt::Display for MountType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bind => write!(f, "bind"),
            Self::Volume => write!(f, "volume"),
            Self::Tmpfs => write!(f, "tmpfs"),
        }
    }
}

/// Port configuration
#[derive(Debug, Clone)]
pub struct PortConfig {
    /// Host port (None for auto-assign)
    pub host_port: Option<u16>,
    /// Container port
    pub container_port: u16,
    /// Protocol (tcp/udp)
    pub protocol: String,
    /// Host IP to bind to
    pub host_ip: Option<String>,
}

/// Exec configuration
#[derive(Debug, Clone, Default)]
pub struct ExecConfig {
    /// Command to execute
    pub cmd: Vec<String>,
    /// Environment variables
    pub env: HashMap<String, String>,
    /// Working directory
    pub working_dir: Option<String>,
    /// User to run as
    pub user: Option<String>,
    /// Allocate TTY
    pub tty: bool,
    /// Attach stdin
    pub stdin: bool,
    /// Privileged mode
    pub privileged: bool,
}

/// Result of exec command
#[derive(Debug)]
pub struct ExecResult {
    /// Exit code
    pub exit_code: i64,
    /// Combined stdout/stderr output
    pub output: String,
}

/// Basic container info for listing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContainerInfo {
    pub id: ContainerId,
    pub name: String,
    pub image: String,
    pub status: ContainerStatus,
    pub created: i64,
    pub labels: HashMap<String, String>,
}

impl ContainerInfo {
    /// Check if this container is managed by devc
    pub fn is_devc_managed(&self) -> bool {
        self.labels.contains_key("devc.managed")
    }

    /// Get the devc project name if set
    pub fn devc_project(&self) -> Option<&str> {
        self.labels.get("devc.project").map(|s| s.as_str())
    }
}

/// Detailed container information
#[derive(Debug, Clone)]
pub struct ContainerDetails {
    pub id: ContainerId,
    pub name: String,
    pub image: String,
    pub image_id: String,
    pub status: ContainerStatus,
    pub created: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub exit_code: Option<i64>,
    pub labels: HashMap<String, String>,
    pub env: Vec<String>,
    pub mounts: Vec<MountInfo>,
    pub ports: Vec<PortInfo>,
    pub network_settings: NetworkSettings,
}

/// Mount information
#[derive(Debug, Clone)]
pub struct MountInfo {
    pub mount_type: String,
    pub source: String,
    pub destination: String,
    pub read_only: bool,
}

/// Port information
#[derive(Debug, Clone)]
pub struct PortInfo {
    pub container_port: u16,
    pub host_port: Option<u16>,
    pub protocol: String,
    pub host_ip: Option<String>,
}

/// Network settings
#[derive(Debug, Clone, Default)]
pub struct NetworkSettings {
    pub ip_address: Option<String>,
    pub gateway: Option<String>,
    pub networks: HashMap<String, NetworkInfo>,
}

/// Network information
#[derive(Debug, Clone)]
pub struct NetworkInfo {
    pub network_id: String,
    pub ip_address: Option<String>,
    pub gateway: Option<String>,
}

/// Log configuration
#[derive(Debug, Clone, Default)]
pub struct LogConfig {
    /// Follow log output
    pub follow: bool,
    /// Show stdout
    pub stdout: bool,
    /// Show stderr
    pub stderr: bool,
    /// Number of lines from end to show
    pub tail: Option<u64>,
    /// Show timestamps
    pub timestamps: bool,
    /// Show logs since this time (unix timestamp)
    pub since: Option<i64>,
    /// Show logs until this time (unix timestamp)
    pub until: Option<i64>,
}

/// Log stream
pub struct LogStream {
    pub stream: Pin<Box<dyn AsyncRead + Send>>,
}

/// Provider information
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    pub provider_type: ProviderType,
    pub version: String,
    pub api_version: String,
    pub os: String,
    pub arch: String,
}

/// Source of a discovered devcontainer
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DevcontainerSource {
    /// Created and managed by devc
    Devc,
    /// Created by VS Code Dev Containers extension
    VsCode,
    /// Created by another tool or manually with devcontainer patterns
    Other,
}

impl std::fmt::Display for DevcontainerSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Devc => write!(f, "devc"),
            Self::VsCode => write!(f, "vscode"),
            Self::Other => write!(f, "other"),
        }
    }
}

/// A discovered devcontainer (may or may not be managed by devc)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredContainer {
    /// Container ID
    pub id: ContainerId,
    /// Container name
    pub name: String,
    /// Image used
    pub image: String,
    /// Container status
    pub status: ContainerStatus,
    /// Whether managed by devc
    pub managed: bool,
    /// Source/creator of this container
    pub source: DevcontainerSource,
    /// Workspace folder path (if detected)
    pub workspace_path: Option<String>,
    /// All labels on the container
    pub labels: HashMap<String, String>,
}
