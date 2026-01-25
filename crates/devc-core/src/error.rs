//! Error types for devc-core

use thiserror::Error;

#[derive(Error, Debug)]
pub enum CoreError {
    #[error("Configuration error: {0}")]
    Config(#[from] devc_config::ConfigError),

    #[error("Provider error: {0}")]
    Provider(#[from] devc_provider::ProviderError),

    #[error("Container not found: {0}")]
    ContainerNotFound(String),

    #[error("Container already exists: {0}")]
    ContainerExists(String),

    #[error("Invalid state: {0}")]
    InvalidState(String),

    #[error("Build failed: {0}")]
    BuildFailed(String),

    #[error("Exec failed: {0}")]
    ExecFailed(String),

    #[error("Dotfiles error: {0}")]
    DotfilesError(String),

    #[error("SSH setup failed: {0}")]
    SshSetupError(String),

    #[error("SSH key generation failed: {0}")]
    SshKeygenError(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("State file corrupted: {0}")]
    StateCorrupted(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
