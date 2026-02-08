//! Container provider trait and implementations for devc
//!
//! This crate provides an abstraction over container runtimes (Docker, Podman)
//! with a consistent API for container operations.
//!
//! Uses CLI-based implementation for:
//! - Simpler implementation
//! - Automatic credential handling (via ~/.docker/config.json)
//! - Proper user context handling (no permissions issues)
//! - Works with Docker alternatives (Colima, Rancher, Lima, OrbStack)

mod cli_provider;
mod error;
mod types;

pub use cli_provider::CliProvider;
pub use error::*;
pub use types::*;

use async_trait::async_trait;
use std::pin::Pin;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

/// Trait for container providers (Docker, Podman, etc.)
#[async_trait]
pub trait ContainerProvider: Send + Sync {
    /// Build an image from a Dockerfile
    async fn build(&self, config: &BuildConfig) -> Result<ImageId>;

    /// Build an image with progress streaming
    /// Progress updates are sent to the provided channel
    async fn build_with_progress(
        &self,
        config: &BuildConfig,
        progress: mpsc::UnboundedSender<String>,
    ) -> Result<ImageId>;

    /// Pull an image from a registry
    async fn pull(&self, image: &str) -> Result<ImageId>;

    /// Create a container from an image
    async fn create(&self, config: &CreateContainerConfig) -> Result<ContainerId>;

    /// Start a container
    async fn start(&self, id: &ContainerId) -> Result<()>;

    /// Stop a container
    async fn stop(&self, id: &ContainerId, timeout: Option<u32>) -> Result<()>;

    /// Remove a container
    async fn remove(&self, id: &ContainerId, force: bool) -> Result<()>;

    /// Remove a container by name (best effort, for cleanup)
    ///
    /// This is used to clean up orphaned containers before creating a new one
    /// with the same name. Errors are ignored since the container may not exist.
    async fn remove_by_name(&self, name: &str) -> Result<()>;

    /// Execute a command in a running container
    async fn exec(&self, id: &ContainerId, config: &ExecConfig) -> Result<ExecResult>;

    /// Execute a command with interactive I/O streams
    async fn exec_interactive(
        &self,
        id: &ContainerId,
        config: &ExecConfig,
    ) -> Result<ExecStream>;

    /// List containers managed by devc
    async fn list(&self, all: bool) -> Result<Vec<ContainerInfo>>;

    /// Get detailed information about a container
    async fn inspect(&self, id: &ContainerId) -> Result<ContainerDetails>;

    /// Get container logs
    async fn logs(&self, id: &ContainerId, config: &LogConfig) -> Result<LogStream>;

    /// Check if the provider is available/connected
    async fn ping(&self) -> Result<()>;

    /// Get provider information
    fn info(&self) -> ProviderInfo;

    /// Get resource usage stats for one or more containers
    ///
    /// Returns stats for all requested containers. Containers that are not running
    /// or cannot provide stats are silently skipped.
    async fn stats(&self, ids: &[&ContainerId]) -> Result<Vec<ContainerStats>>;

    /// Discover all devcontainers (including those not managed by devc)
    /// Returns containers with devcontainer-related labels or mounts
    async fn discover_devcontainers(&self) -> Result<Vec<DiscoveredContainer>>;

    /// Copy files into a container
    async fn copy_into(
        &self,
        id: &ContainerId,
        src: &std::path::Path,
        dest: &str,
    ) -> Result<()>;

    /// Copy files from a container
    async fn copy_from(
        &self,
        id: &ContainerId,
        src: &str,
        dest: &std::path::Path,
    ) -> Result<()>;

    /// Start services defined in Docker Compose files
    async fn compose_up(
        &self,
        compose_files: &[&str],
        project_name: &str,
        project_dir: &std::path::Path,
        progress: Option<mpsc::UnboundedSender<String>>,
    ) -> Result<()>;

    /// Stop and remove services defined in Docker Compose files
    async fn compose_down(
        &self,
        compose_files: &[&str],
        project_name: &str,
        project_dir: &std::path::Path,
    ) -> Result<()>;

    /// List services in a Docker Compose project
    async fn compose_ps(
        &self,
        compose_files: &[&str],
        project_name: &str,
        project_dir: &std::path::Path,
    ) -> Result<Vec<ComposeServiceInfo>>;
}

/// Interactive exec stream with stdin/stdout/stderr
pub struct ExecStream {
    pub stdin: Option<Pin<Box<dyn AsyncWrite + Send>>>,
    pub output: Pin<Box<dyn AsyncRead + Send>>,
    pub id: String,
}

/// Factory function to create a provider based on type
pub async fn create_provider(
    provider_type: ProviderType,
    _config: &devc_config::GlobalConfig,
) -> Result<Box<dyn ContainerProvider>> {
    match provider_type {
        ProviderType::Docker => {
            let provider = CliProvider::new_docker().await?;
            Ok(Box::new(provider))
        }
        ProviderType::Podman => {
            let provider = CliProvider::new_podman().await?;
            Ok(Box::new(provider))
        }
    }
}

/// Test if a specific provider is available and responsive
/// Returns Ok(true) if connected, Ok(false) if not available, Err on unexpected error
pub async fn test_provider_connectivity(
    provider_type: ProviderType,
    config: &devc_config::GlobalConfig,
) -> Result<bool> {
    match create_provider(provider_type, config).await {
        Ok(provider) => match provider.ping().await {
            Ok(()) => Ok(true),
            Err(_) => Ok(false),
        },
        Err(_) => Ok(false),
    }
}

/// Detect which providers are available on the system
/// Returns a list of (ProviderType, is_available) pairs
/// Tests Docker first, then Podman
pub async fn detect_available_providers(
    config: &devc_config::GlobalConfig,
) -> Vec<(ProviderType, bool)> {
    // Test both providers in parallel
    let (docker_result, podman_result) = tokio::join!(
        test_provider_connectivity(ProviderType::Docker, config),
        test_provider_connectivity(ProviderType::Podman, config)
    );

    vec![
        (ProviderType::Docker, docker_result.unwrap_or(false)),
        (ProviderType::Podman, podman_result.unwrap_or(false)),
    ]
}

/// Check if we're running inside a Fedora Toolbox or similar container
#[cfg(target_os = "linux")]
pub fn is_in_toolbox() -> bool {
    std::path::Path::new("/run/.containerenv").exists()
}

/// Non-Linux stub: never in a toolbox
#[cfg(not(target_os = "linux"))]
pub fn is_in_toolbox() -> bool {
    false
}

/// Create the default provider based on global config
/// Auto-detects Toolbox environment and uses host podman if needed
/// If provider is not configured (empty), auto-detects by trying Docker first, then Podman
pub async fn create_default_provider(
    config: &devc_config::GlobalConfig,
) -> Result<Box<dyn ContainerProvider>> {
    // Only check for toolbox on Linux
    #[cfg(target_os = "linux")]
    if is_in_toolbox() {
        tracing::info!("Detected toolbox environment, using host podman");
        match CliProvider::new_toolbox().await {
            Ok(provider) => return Ok(Box::new(provider)),
            Err(e) => {
                tracing::warn!("Failed to connect to host podman: {}, trying direct", e);
            }
        }
    }

    // Determine provider type - auto-detect if empty
    let provider_type = match config.defaults.provider.as_str() {
        "podman" => ProviderType::Podman,
        "docker" => ProviderType::Docker,
        "" => {
            // Auto-detect: try Docker first (more common on Windows/Mac), then Podman
            tracing::info!("No provider configured, auto-detecting...");
            let available = detect_available_providers(config).await;

            // Find first available provider (Docker is first in list)
            let detected = available.iter().find(|(_, available)| *available);

            match detected {
                Some((provider_type, _)) => {
                    tracing::info!("Auto-detected provider: {}", provider_type);
                    *provider_type
                }
                None => {
                    // Neither available, default to Docker for better error messages
                    tracing::warn!("No providers detected, defaulting to Docker");
                    ProviderType::Docker
                }
            }
        }
        _ => ProviderType::Docker, // Unknown provider, default to Docker
    };

    match create_provider(provider_type, config).await {
        Ok(provider) => Ok(provider),
        Err(e) => {
            // If in toolbox and direct connection failed, give a helpful error (Linux only)
            #[cfg(target_os = "linux")]
            if is_in_toolbox() {
                return Err(ProviderError::ConnectionError(format!(
                    "Cannot connect to container runtime. In toolbox, ensure 'flatpak-spawn --host podman' works. Error: {}",
                    e
                )));
            }

            Err(ProviderError::ConnectionError(format_connection_error(
                provider_type,
                &e,
            )))
        }
    }
}

/// Format a helpful connection error message with actionable instructions
fn format_connection_error(provider: ProviderType, underlying: &ProviderError) -> String {
    let provider_name = match provider {
        ProviderType::Podman => "Podman",
        ProviderType::Docker => "Docker",
    };

    let mut msg = format!("Cannot connect to {}\n\n", provider_name);
    msg.push_str(&format!("Underlying error: {}\n\n", underlying));

    match provider {
        ProviderType::Podman => {
            msg.push_str("To install Podman:\n");
            msg.push_str("  # On Fedora/RHEL:\n");
            msg.push_str("  sudo dnf install podman\n\n");
            msg.push_str("  # On Ubuntu/Debian:\n");
            msg.push_str("  sudo apt install podman\n\n");
            msg.push_str("To verify it's working:\n");
            msg.push_str("  podman --version\n");
        }
        ProviderType::Docker => {
            msg.push_str("To install Docker:\n");
            msg.push_str("  Visit https://docs.docker.com/get-docker/\n\n");
            msg.push_str("To start Docker:\n");
            msg.push_str("  sudo systemctl enable --now docker\n\n");
            msg.push_str("To verify it's working:\n");
            msg.push_str("  docker --version\n");
        }
    }

    msg
}
