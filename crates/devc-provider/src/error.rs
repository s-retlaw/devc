//! Error types for container providers

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("Failed to connect to container runtime: {0}")]
    ConnectionError(String),

    #[error("Container not found: {0}")]
    ContainerNotFound(String),

    #[error("Image not found: {0}")]
    ImageNotFound(String),

    #[error("Build failed: {0}")]
    BuildError(String),

    #[error("Exec failed: {0}")]
    ExecError(String),

    #[error("Container runtime error: {0}")]
    RuntimeError(String),

    #[error("Invalid configuration: {0}")]
    ConfigError(String),

    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Timeout waiting for operation")]
    Timeout,

    #[error("Operation cancelled")]
    Cancelled,

    #[error("Unsupported operation: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, ProviderError>;
