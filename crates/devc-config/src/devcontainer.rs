//! devcontainer.json configuration parsing
//!
//! Supports the VSCode devcontainer.json specification

use crate::{ConfigError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Complete devcontainer.json configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DevContainerConfig {
    /// Name of the dev container
    pub name: Option<String>,

    // Image-based configuration
    /// Docker image to use
    pub image: Option<String>,

    // Dockerfile-based configuration
    /// Path to Dockerfile (relative to .devcontainer)
    #[serde(alias = "dockerFile")]
    pub dockerfile: Option<String>,

    /// Build configuration
    pub build: Option<BuildConfig>,

    /// Docker Compose configuration
    pub docker_compose_file: Option<StringOrArray>,

    /// Service name when using Docker Compose
    pub service: Option<String>,

    // Container configuration
    /// Arguments to pass to docker run
    pub run_args: Option<Vec<String>>,

    /// Environment variables for the container
    pub container_env: Option<HashMap<String, String>>,

    /// User to run as in the container
    pub remote_user: Option<String>,

    /// Container user
    pub container_user: Option<String>,

    /// Working directory inside the container
    pub workspace_folder: Option<String>,

    /// Mounts to add to the container
    pub mounts: Option<Vec<Mount>>,

    /// Ports to forward
    pub forward_ports: Option<Vec<PortMapping>>,

    /// App ports (ports that are always forwarded)
    pub app_port: Option<IntOrArray>,

    // Lifecycle commands
    /// Command to run after container is created
    pub post_create_command: Option<Command>,

    /// Command to run after container starts
    pub post_start_command: Option<Command>,

    /// Command to run when attaching to container
    pub post_attach_command: Option<Command>,

    /// Command to run before container is created
    pub init_command: Option<Command>,

    /// Command to run when container is created (runs before postCreateCommand)
    pub on_create_command: Option<Command>,

    /// Command to update container contents
    pub update_content_command: Option<Command>,

    /// Wait for commands to complete
    pub wait_for: Option<String>,

    // Features
    /// devcontainer features to install
    pub features: Option<HashMap<String, FeatureConfig>>,

    // VSCode specific (we parse but may not use all)
    /// VSCode extensions to install
    pub customizations: Option<Customizations>,

    /// Deprecated: extensions field
    pub extensions: Option<Vec<String>>,

    /// Deprecated: settings field
    pub settings: Option<serde_json::Value>,

    // Devc-specific extensions (custom fields)
    /// Pre-build scripts (devc extension)
    #[serde(rename = "devc.preBuildScripts")]
    pub pre_build_scripts: Option<Vec<String>>,

    /// Post-build scripts (devc extension)
    #[serde(rename = "devc.postBuildScripts")]
    pub post_build_scripts: Option<Vec<String>>,

    /// Dotfiles configuration (devc extension)
    #[serde(rename = "devc.dotfiles")]
    pub dotfiles: Option<DotfilesConfig>,

    /// Additional options we don't explicitly handle
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Build configuration
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BuildConfig {
    /// Path to Dockerfile
    pub dockerfile: Option<String>,

    /// Build context path
    pub context: Option<String>,

    /// Build arguments
    pub args: Option<HashMap<String, String>>,

    /// Target stage in multi-stage build
    pub target: Option<String>,

    /// Cache from images
    pub cache_from: Option<StringOrArray>,

    /// Additional options
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Mount configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Mount {
    /// String format: "type=bind,source=/path,target=/path"
    String(String),
    /// Object format
    Object(MountObject),
}

/// Mount object configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MountObject {
    /// Mount type (bind, volume, tmpfs)
    #[serde(rename = "type")]
    pub mount_type: Option<String>,
    /// Source path
    pub source: Option<String>,
    /// Target path in container
    pub target: String,
    /// Read-only mount
    pub read_only: Option<bool>,
}

/// Port mapping configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PortMapping {
    /// Simple port number
    Number(u16),
    /// Object with label
    Object(PortObject),
}

/// Port object configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortObject {
    pub port: u16,
    pub label: Option<String>,
    pub protocol: Option<String>,
    pub on_auto_forward: Option<String>,
}

/// Command can be a string, array, or object with parallel commands
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Command {
    /// Single command string
    String(String),
    /// Array of command parts
    Array(Vec<String>),
    /// Object with named commands (run in parallel)
    Object(HashMap<String, StringOrArray>),
}

/// String or array of strings
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StringOrArray {
    String(String),
    Array(Vec<String>),
}

/// Integer or array of integers
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IntOrArray {
    Int(u16),
    Array(Vec<u16>),
}

/// Feature configuration - can be boolean, string, or object
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FeatureConfig {
    /// Enable/disable feature
    Bool(bool),
    /// Feature version
    Version(String),
    /// Full feature options
    Options(HashMap<String, serde_json::Value>),
}

/// VSCode customizations
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Customizations {
    pub vscode: Option<VsCodeCustomizations>,
    #[serde(flatten)]
    pub other: HashMap<String, serde_json::Value>,
}

/// VSCode-specific customizations
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VsCodeCustomizations {
    pub extensions: Option<Vec<String>>,
    pub settings: Option<serde_json::Value>,
}

/// Dotfiles configuration (devc extension)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DotfilesConfig {
    /// Repository URL
    pub repository: Option<String>,
    /// Local path
    pub local_path: Option<String>,
    /// Install command
    pub install_command: Option<String>,
    /// Target path in container
    pub target_path: Option<String>,
}

impl DevContainerConfig {
    /// Load devcontainer.json from a directory
    ///
    /// Searches for configuration in standard locations:
    /// 1. `.devcontainer/devcontainer.json`
    /// 2. `.devcontainer.json`
    /// 3. `.devcontainer/<folder>/devcontainer.json` (returns first found)
    pub fn load_from_dir(dir: &Path) -> Result<(Self, PathBuf)> {
        let candidates = [
            dir.join(".devcontainer/devcontainer.json"),
            dir.join(".devcontainer.json"),
        ];

        for path in &candidates {
            if path.exists() {
                let config = Self::load_from(path)?;
                return Ok((config, path.clone()));
            }
        }

        // Check for subdirectories in .devcontainer
        let devcontainer_dir = dir.join(".devcontainer");
        if devcontainer_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&devcontainer_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        let config_path = path.join("devcontainer.json");
                        if config_path.exists() {
                            let config = Self::load_from(&config_path)?;
                            return Ok((config, config_path));
                        }
                    }
                }
            }
        }

        Err(ConfigError::NotFound(dir.join(".devcontainer")))
    }

    /// Load devcontainer.json from a specific file
    pub fn load_from(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| ConfigError::ReadError {
            path: path.to_path_buf(),
            source: e,
        })?;

        Self::parse(&content, path)
    }

    /// Parse devcontainer.json content
    pub fn parse(content: &str, path: &Path) -> Result<Self> {
        // Strip comments (devcontainer.json supports JSONC)
        let content = strip_json_comments(content);

        serde_json::from_str(&content).map_err(|e| ConfigError::JsonParseError {
            path: path.to_path_buf(),
            source: e,
        })
    }

    /// Get the effective image source (image, dockerfile, or compose)
    pub fn image_source(&self) -> ImageSource {
        if let Some(ref image) = self.image {
            ImageSource::Image(image.clone())
        } else if let Some(ref dockerfile) = self.dockerfile {
            ImageSource::Dockerfile {
                path: dockerfile.clone(),
                context: None,
                args: None,
            }
        } else if let Some(ref build) = self.build {
            ImageSource::Dockerfile {
                path: build.dockerfile.clone().unwrap_or_else(|| "Dockerfile".to_string()),
                context: build.context.clone(),
                args: build.args.clone(),
            }
        } else if self.docker_compose_file.is_some() {
            ImageSource::Compose
        } else {
            ImageSource::None
        }
    }

    /// Get the remote user (with fallback)
    pub fn effective_user(&self) -> Option<&str> {
        self.remote_user
            .as_deref()
            .or(self.container_user.as_deref())
    }

    /// Get all forward ports as a flat list
    pub fn forward_ports_list(&self) -> Vec<u16> {
        let mut ports = Vec::new();

        if let Some(ref forward) = self.forward_ports {
            for mapping in forward {
                match mapping {
                    PortMapping::Number(p) => ports.push(*p),
                    PortMapping::Object(obj) => ports.push(obj.port),
                }
            }
        }

        if let Some(ref app) = self.app_port {
            match app {
                IntOrArray::Int(p) => ports.push(*p),
                IntOrArray::Array(arr) => ports.extend(arr),
            }
        }

        ports
    }
}

/// Image source type
#[derive(Debug, Clone)]
pub enum ImageSource {
    /// Pre-built image
    Image(String),
    /// Build from Dockerfile
    Dockerfile {
        path: String,
        context: Option<String>,
        args: Option<HashMap<String, String>>,
    },
    /// Docker Compose
    Compose,
    /// No image source specified
    None,
}

/// Strip JSON comments (// and /* */) for JSONC support
fn strip_json_comments(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escape_next = false;

    while let Some(c) = chars.next() {
        if escape_next {
            result.push(c);
            escape_next = false;
            continue;
        }

        if c == '\\' && in_string {
            result.push(c);
            escape_next = true;
            continue;
        }

        if c == '"' {
            in_string = !in_string;
            result.push(c);
            continue;
        }

        if in_string {
            result.push(c);
            continue;
        }

        if c == '/' {
            if let Some(&next) = chars.peek() {
                if next == '/' {
                    // Line comment - skip to end of line
                    chars.next();
                    while let Some(&nc) = chars.peek() {
                        if nc == '\n' {
                            break;
                        }
                        chars.next();
                    }
                    continue;
                } else if next == '*' {
                    // Block comment - skip to */
                    chars.next();
                    while let Some(nc) = chars.next() {
                        if nc == '*' {
                            if let Some(&'/' ) = chars.peek() {
                                chars.next();
                                break;
                            }
                        }
                    }
                    continue;
                }
            }
        }

        result.push(c);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_image() {
        let json = r#"{"image": "mcr.microsoft.com/devcontainers/rust:1"}"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            config.image,
            Some("mcr.microsoft.com/devcontainers/rust:1".to_string())
        );
    }

    #[test]
    fn test_parse_with_features() {
        let json = r#"{
            "image": "ubuntu:22.04",
            "features": {
                "ghcr.io/devcontainers/features/git:1": {},
                "ghcr.io/devcontainers/features/node:1": {
                    "version": "18"
                }
            }
        }"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        assert!(config.features.is_some());
        assert_eq!(config.features.as_ref().unwrap().len(), 2);
    }

    #[test]
    fn test_parse_with_build() {
        let json = r#"{
            "build": {
                "dockerfile": "Dockerfile",
                "context": "..",
                "args": {
                    "VARIANT": "3.11"
                }
            }
        }"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        assert!(config.build.is_some());
        let build = config.build.unwrap();
        assert_eq!(build.dockerfile, Some("Dockerfile".to_string()));
    }

    #[test]
    fn test_strip_comments() {
        let input = r#"{
            // This is a comment
            "name": "test", /* inline comment */
            "image": "ubuntu"
        }"#;
        let stripped = strip_json_comments(input);
        let config: DevContainerConfig = serde_json::from_str(&stripped).unwrap();
        assert_eq!(config.name, Some("test".to_string()));
    }

    #[test]
    fn test_command_variants() {
        // String command
        let json = r#"{"postCreateCommand": "npm install"}"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config.post_create_command, Some(Command::String(_))));

        // Array command
        let json = r#"{"postCreateCommand": ["npm", "install"]}"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config.post_create_command, Some(Command::Array(_))));
    }
}
