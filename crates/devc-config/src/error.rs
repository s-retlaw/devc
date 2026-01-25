//! Error types for configuration parsing

use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Failed to read config file at {path}: {source}")]
    ReadError {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("Failed to parse TOML config at {path}: {source}")]
    TomlParseError {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error("Failed to parse JSON config at {path}: {source}")]
    JsonParseError {
        path: PathBuf,
        source: serde_json::Error,
    },

    #[error("Config file not found: {0}")]
    NotFound(PathBuf),

    #[error("Invalid configuration: {0}")]
    Invalid(String),

    #[error("Failed to determine config directory")]
    NoConfigDir,

    #[error("Failed to determine data directory")]
    NoDataDir,

    #[error("Failed to write config file at {path}: {source}")]
    WriteError {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, ConfigError>;
