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

    /// Per-port attributes (label, protocol, onAutoForward)
    pub ports_attributes: Option<HashMap<String, PortAttributesEntry>>,

    // Lifecycle commands
    /// Command to run after container is created
    pub post_create_command: Option<Command>,

    /// Command to run after container starts
    pub post_start_command: Option<Command>,

    /// Command to run when attaching to container
    pub post_attach_command: Option<Command>,

    /// Command to run on the host before container is created
    #[serde(alias = "initCommand")]
    pub initialize_command: Option<Command>,

    /// Command to run when container is created (runs before postCreateCommand)
    pub on_create_command: Option<Command>,

    /// Command to update container contents
    pub update_content_command: Option<Command>,

    /// Wait for commands to complete
    pub wait_for: Option<String>,

    /// Run an init process (PID 1) inside the container
    pub init: Option<bool>,

    /// Run container in privileged mode
    pub privileged: Option<bool>,

    /// Linux capabilities to add
    pub cap_add: Option<Vec<String>>,

    /// Security options
    pub security_opt: Option<Vec<String>>,

    /// Whether to override the default command
    pub override_command: Option<bool>,

    /// Environment variables for tools running in the container (not set at container creation)
    pub remote_env: Option<HashMap<String, String>>,

    /// Action to take when the tool is closed
    pub shutdown_action: Option<String>,

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Mount {
    /// String format: "type=bind,source=/path,target=/path"
    String(String),
    /// Object format
    Object(MountObject),
}

/// Mount object configuration
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Load ALL devcontainer.json configs from a directory
    ///
    /// Returns all valid configs found in standard locations:
    /// 1. `.devcontainer/devcontainer.json`
    /// 2. `.devcontainer.json`
    /// 3. `.devcontainer/<folder>/devcontainer.json` (all subdirs, sorted by name)
    ///
    /// Invalid configs are skipped with a warning. Returns an empty Vec if none found.
    pub fn load_all_from_dir(dir: &Path) -> Vec<(Self, PathBuf)> {
        let mut results = Vec::new();

        // Check top-level candidates
        let candidates = [
            dir.join(".devcontainer/devcontainer.json"),
            dir.join(".devcontainer.json"),
        ];

        for path in &candidates {
            if path.exists() {
                match Self::load_from(path) {
                    Ok(config) => results.push((config, path.clone())),
                    Err(e) => tracing::warn!("Skipping invalid config {}: {}", path.display(), e),
                }
            }
        }

        // Check subdirectories in .devcontainer (sorted for determinism)
        let devcontainer_dir = dir.join(".devcontainer");
        if devcontainer_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&devcontainer_dir) {
                let mut subdirs: Vec<_> = entries
                    .flatten()
                    .filter(|e| e.path().is_dir())
                    .collect();
                subdirs.sort_by_key(|e| e.file_name());

                for entry in subdirs {
                    let config_path = entry.path().join("devcontainer.json");
                    if config_path.exists() {
                        match Self::load_from(&config_path) {
                            Ok(config) => results.push((config, config_path)),
                            Err(e) => tracing::warn!(
                                "Skipping invalid config {}: {}",
                                config_path.display(),
                                e
                            ),
                        }
                    }
                }
            }
        }

        results
    }

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

    /// Get auto-forward configuration for ports declared in the devcontainer config.
    ///
    /// Returns a list of `PortForwardConfig` from `forwardPorts`, `appPort`, and `portsAttributes`:
    /// - `forwardPorts` numeric entries default to `Notify`
    /// - `forwardPorts` object entries map `onAutoForward` to the enum, carrying label/protocol
    /// - `appPort` entries always use `Silent` (always forwarded quietly)
    /// - `portsAttributes` entries override/supplement label, protocol, and action
    pub fn auto_forward_config(&self) -> Vec<PortForwardConfig> {
        let mut result = Vec::new();

        if let Some(ref forward) = self.forward_ports {
            for mapping in forward {
                match mapping {
                    PortMapping::Number(p) => {
                        result.push(PortForwardConfig {
                            port: *p,
                            action: AutoForwardAction::Notify,
                            label: None,
                            protocol: None,
                        });
                    }
                    PortMapping::Object(obj) => {
                        result.push(PortForwardConfig {
                            port: obj.port,
                            action: parse_auto_forward_action(obj.on_auto_forward.as_deref()),
                            label: obj.label.clone(),
                            protocol: obj.protocol.clone(),
                        });
                    }
                }
            }
        }

        if let Some(ref app) = self.app_port {
            match app {
                IntOrArray::Int(p) => {
                    result.push(PortForwardConfig {
                        port: *p,
                        action: AutoForwardAction::Silent,
                        label: None,
                        protocol: None,
                    });
                }
                IntOrArray::Array(arr) => {
                    for p in arr {
                        result.push(PortForwardConfig {
                            port: *p,
                            action: AutoForwardAction::Silent,
                            label: None,
                            protocol: None,
                        });
                    }
                }
            }
        }

        // Merge portsAttributes overrides
        if let Some(ref attrs) = self.ports_attributes {
            for (key, entry) in attrs {
                let port: u16 = match key.parse() {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                if let Some(existing) = result.iter_mut().find(|c| c.port == port) {
                    if let Some(ref label) = entry.label {
                        existing.label = Some(label.clone());
                    }
                    if let Some(ref protocol) = entry.protocol {
                        existing.protocol = Some(protocol.clone());
                    }
                    if entry.on_auto_forward.is_some() {
                        existing.action =
                            parse_auto_forward_action(entry.on_auto_forward.as_deref());
                    }
                } else {
                    result.push(PortForwardConfig {
                        port,
                        action: parse_auto_forward_action(entry.on_auto_forward.as_deref()),
                        label: entry.label.clone(),
                        protocol: entry.protocol.clone(),
                    });
                }
            }
        }

        result
    }

    /// Apply variable substitution to all string fields that support it
    pub fn substitute_variables(&mut self, ctx: &crate::SubstitutionContext) {
        use crate::substitute::{substitute, substitute_map, substitute_opt, substitute_vec};

        self.workspace_folder = substitute_opt(&self.workspace_folder, ctx);

        if let Some(ref env) = self.container_env {
            self.container_env = Some(substitute_map(env, ctx));
        }
        if let Some(ref env) = self.remote_env {
            self.remote_env = Some(substitute_map(env, ctx));
        }

        if let Some(ref args) = self.run_args {
            self.run_args = Some(substitute_vec(args, ctx));
        }

        // Substitute in mounts
        if let Some(ref mut mounts) = self.mounts {
            for mount in mounts.iter_mut() {
                match mount {
                    Mount::String(s) => *s = substitute(s, ctx),
                    Mount::Object(obj) => {
                        obj.source = substitute_opt(&obj.source, ctx);
                        obj.target = substitute(&obj.target, ctx);
                    }
                }
            }
        }

        // Substitute in lifecycle commands
        fn substitute_command(cmd: &mut Command, ctx: &crate::SubstitutionContext) {
            use crate::substitute::substitute;
            match cmd {
                Command::String(s) => *s = substitute(s, ctx),
                Command::Array(arr) => {
                    for s in arr.iter_mut() {
                        *s = substitute(s, ctx);
                    }
                }
                Command::Object(map) => {
                    for value in map.values_mut() {
                        match value {
                            StringOrArray::String(s) => *s = substitute(s, ctx),
                            StringOrArray::Array(arr) => {
                                for s in arr.iter_mut() {
                                    *s = substitute(s, ctx);
                                }
                            }
                        }
                    }
                }
            }
        }

        if let Some(ref mut cmd) = self.initialize_command {
            substitute_command(cmd, ctx);
        }
        if let Some(ref mut cmd) = self.on_create_command {
            substitute_command(cmd, ctx);
        }
        if let Some(ref mut cmd) = self.update_content_command {
            substitute_command(cmd, ctx);
        }
        if let Some(ref mut cmd) = self.post_create_command {
            substitute_command(cmd, ctx);
        }
        if let Some(ref mut cmd) = self.post_start_command {
            substitute_command(cmd, ctx);
        }
        if let Some(ref mut cmd) = self.post_attach_command {
            substitute_command(cmd, ctx);
        }
    }
}

/// Action to take when a port is auto-forwarded
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoForwardAction {
    /// Notify the user when the port is forwarded
    Notify,
    /// Forward silently without notification
    Silent,
    /// Do not auto-forward this port
    Ignore,
    /// Open in browser after forwarding (every time)
    OpenBrowser,
    /// Open in browser after forwarding (only the first time)
    OpenBrowserOnce,
}

/// Configuration for a single auto-forwarded port
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortForwardConfig {
    pub port: u16,
    pub action: AutoForwardAction,
    pub label: Option<String>,
    pub protocol: Option<String>,
}

/// Attributes for a port from the `portsAttributes` field
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PortAttributesEntry {
    pub label: Option<String>,
    pub protocol: Option<String>,
    pub on_auto_forward: Option<String>,
}

/// Parse an `onAutoForward` string into an `AutoForwardAction`.
fn parse_auto_forward_action(value: Option<&str>) -> AutoForwardAction {
    match value {
        Some("silent") => AutoForwardAction::Silent,
        Some("ignore") => AutoForwardAction::Ignore,
        Some("openBrowser") => AutoForwardAction::OpenBrowser,
        Some("openBrowserOnce") => AutoForwardAction::OpenBrowserOnce,
        _ => AutoForwardAction::Notify,
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

    #[test]
    fn test_parse_runtime_flags() {
        let json = r#"{
            "image": "ubuntu:22.04",
            "init": true,
            "privileged": false,
            "capAdd": ["SYS_PTRACE", "NET_ADMIN"],
            "securityOpt": ["seccomp=unconfined"],
            "overrideCommand": false,
            "remoteEnv": {"EDITOR": "vim"},
            "shutdownAction": "stopContainer"
        }"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.init, Some(true));
        assert_eq!(config.privileged, Some(false));
        assert_eq!(config.cap_add.as_ref().unwrap().len(), 2);
        assert_eq!(config.security_opt.as_ref().unwrap()[0], "seccomp=unconfined");
        assert_eq!(config.override_command, Some(false));
        assert_eq!(config.remote_env.as_ref().unwrap().get("EDITOR").unwrap(), "vim");
        assert_eq!(config.shutdown_action, Some("stopContainer".to_string()));
    }

    #[test]
    fn test_initialize_command_alias() {
        // initializeCommand (spec name)
        let json = r#"{"initializeCommand": "echo hello"}"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config.initialize_command, Some(Command::String(_))));

        // initCommand (alias for backward compat)
        let json = r#"{"initCommand": "echo hello"}"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        assert!(matches!(config.initialize_command, Some(Command::String(_))));
    }

    #[test]
    fn test_override_command_false() {
        let json = r#"{"image": "ubuntu:22.04", "overrideCommand": false}"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.override_command, Some(false));
    }

    /// Helper to build a PortForwardConfig concisely in tests
    fn pfc(port: u16, action: AutoForwardAction, label: Option<&str>, protocol: Option<&str>) -> PortForwardConfig {
        PortForwardConfig {
            port,
            action,
            label: label.map(String::from),
            protocol: protocol.map(String::from),
        }
    }

    #[test]
    fn test_auto_forward_config_numeric_ports() {
        let json = r#"{"forwardPorts": [3000, 8080]}"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        let fwd = config.auto_forward_config();
        assert_eq!(fwd.len(), 2);
        assert_eq!(fwd[0], pfc(3000, AutoForwardAction::Notify, None, None));
        assert_eq!(fwd[1], pfc(8080, AutoForwardAction::Notify, None, None));
    }

    #[test]
    fn test_auto_forward_config_object_ports() {
        let json = r#"{"forwardPorts": [
            {"port": 3000, "onAutoForward": "silent"},
            {"port": 8080, "onAutoForward": "ignore"},
            {"port": 9090, "onAutoForward": "notify"}
        ]}"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        let fwd = config.auto_forward_config();
        assert_eq!(fwd.len(), 3);
        assert_eq!(fwd[0], pfc(3000, AutoForwardAction::Silent, None, None));
        assert_eq!(fwd[1], pfc(8080, AutoForwardAction::Ignore, None, None));
        assert_eq!(fwd[2], pfc(9090, AutoForwardAction::Notify, None, None));
    }

    #[test]
    fn test_auto_forward_config_app_port() {
        let json = r#"{"appPort": [4000, 5000]}"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        let fwd = config.auto_forward_config();
        assert_eq!(fwd.len(), 2);
        assert_eq!(fwd[0], pfc(4000, AutoForwardAction::Silent, None, None));
        assert_eq!(fwd[1], pfc(5000, AutoForwardAction::Silent, None, None));
    }

    #[test]
    fn test_auto_forward_config_app_port_single() {
        let json = r#"{"appPort": 3000}"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        let fwd = config.auto_forward_config();
        assert_eq!(fwd.len(), 1);
        assert_eq!(fwd[0], pfc(3000, AutoForwardAction::Silent, None, None));
    }

    #[test]
    fn test_auto_forward_config_combined() {
        let json = r#"{"forwardPorts": [3000], "appPort": 8080}"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        let fwd = config.auto_forward_config();
        assert_eq!(fwd.len(), 2);
        assert_eq!(fwd[0], pfc(3000, AutoForwardAction::Notify, None, None));
        assert_eq!(fwd[1], pfc(8080, AutoForwardAction::Silent, None, None));
    }

    #[test]
    fn test_auto_forward_config_empty() {
        let json = r#"{"image": "ubuntu:22.04"}"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        let fwd = config.auto_forward_config();
        assert!(fwd.is_empty());
    }

    #[test]
    fn test_auto_forward_config_open_browser() {
        let json = r#"{"forwardPorts": [
            {"port": 3000, "onAutoForward": "openBrowser"},
            {"port": 8080, "onAutoForward": "openBrowserOnce"}
        ]}"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        let fwd = config.auto_forward_config();
        assert_eq!(fwd.len(), 2);
        assert_eq!(fwd[0], pfc(3000, AutoForwardAction::OpenBrowser, None, None));
        assert_eq!(fwd[1], pfc(8080, AutoForwardAction::OpenBrowserOnce, None, None));
    }

    #[test]
    fn test_auto_forward_config_label_and_protocol() {
        let json = r#"{"forwardPorts": [
            {"port": 3000, "label": "App", "protocol": "https"},
            {"port": 8080, "label": "API"}
        ]}"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        let fwd = config.auto_forward_config();
        assert_eq!(fwd.len(), 2);
        assert_eq!(fwd[0], pfc(3000, AutoForwardAction::Notify, Some("App"), Some("https")));
        assert_eq!(fwd[1], pfc(8080, AutoForwardAction::Notify, Some("API"), None));
    }

    #[test]
    fn test_ports_attributes_merges_existing() {
        let json = r#"{
            "forwardPorts": [3000, 8080],
            "portsAttributes": {
                "3000": {"label": "Frontend", "protocol": "https", "onAutoForward": "silent"},
                "8080": {"label": "Backend"}
            }
        }"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        let fwd = config.auto_forward_config();
        assert_eq!(fwd.len(), 2);
        assert_eq!(fwd[0], pfc(3000, AutoForwardAction::Silent, Some("Frontend"), Some("https")));
        assert_eq!(fwd[1], pfc(8080, AutoForwardAction::Notify, Some("Backend"), None));
    }

    #[test]
    fn test_ports_attributes_adds_new_port() {
        let json = r#"{
            "forwardPorts": [3000],
            "portsAttributes": {
                "9090": {"label": "Metrics", "onAutoForward": "openBrowser"}
            }
        }"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        let fwd = config.auto_forward_config();
        assert_eq!(fwd.len(), 2);
        assert_eq!(fwd[0], pfc(3000, AutoForwardAction::Notify, None, None));
        assert_eq!(fwd[1], pfc(9090, AutoForwardAction::OpenBrowser, Some("Metrics"), None));
    }

    #[test]
    fn test_ports_attributes_overrides_forward_ports_label() {
        let json = r#"{
            "forwardPorts": [{"port": 3000, "label": "Old Label"}],
            "portsAttributes": {
                "3000": {"label": "New Label"}
            }
        }"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        let fwd = config.auto_forward_config();
        assert_eq!(fwd.len(), 1);
        assert_eq!(fwd[0], pfc(3000, AutoForwardAction::Notify, Some("New Label"), None));
    }

    #[test]
    fn test_load_nonexistent_devcontainer_fails() {
        let result = DevContainerConfig::load_from(std::path::Path::new(
            "/tmp/nonexistent_devc_devcontainer.json",
        ));
        assert!(result.is_err());
    }

    #[test]
    fn test_lifecycle_all_hooks() {
        let json = r#"{
            "image": "ubuntu:22.04",
            "initializeCommand": "echo init",
            "onCreateCommand": "echo create",
            "updateContentCommand": "echo update",
            "postCreateCommand": "echo post-create",
            "postStartCommand": "echo post-start",
            "postAttachCommand": "echo post-attach"
        }"#;
        let config: DevContainerConfig = serde_json::from_str(json).unwrap();
        assert!(config.initialize_command.is_some());
        assert!(config.on_create_command.is_some());
        assert!(config.update_content_command.is_some());
        assert!(config.post_create_command.is_some());
        assert!(config.post_start_command.is_some());
        assert!(config.post_attach_command.is_some());
    }

    #[test]
    fn test_load_all_from_dir_multiple_subdirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dc = tmp.path().join(".devcontainer");
        std::fs::create_dir_all(dc.join("python")).unwrap();
        std::fs::create_dir_all(dc.join("node")).unwrap();
        std::fs::write(
            dc.join("devcontainer.json"),
            r#"{"image": "ubuntu:22.04"}"#,
        )
        .unwrap();
        std::fs::write(
            dc.join("python/devcontainer.json"),
            r#"{"image": "python:3.12"}"#,
        )
        .unwrap();
        std::fs::write(
            dc.join("node/devcontainer.json"),
            r#"{"image": "node:20"}"#,
        )
        .unwrap();

        let results = DevContainerConfig::load_all_from_dir(tmp.path());
        assert_eq!(results.len(), 3);
        // First should be .devcontainer/devcontainer.json
        assert!(results[0].1.ends_with(".devcontainer/devcontainer.json"));
        // Subdirs should be sorted: node before python
        assert!(results[1].1.ends_with("node/devcontainer.json"));
        assert!(results[2].1.ends_with("python/devcontainer.json"));
    }

    #[test]
    fn test_load_all_from_dir_single() {
        let tmp = tempfile::tempdir().unwrap();
        let dc = tmp.path().join(".devcontainer");
        std::fs::create_dir_all(&dc).unwrap();
        std::fs::write(
            dc.join("devcontainer.json"),
            r#"{"image": "ubuntu:22.04"}"#,
        )
        .unwrap();

        let results = DevContainerConfig::load_all_from_dir(tmp.path());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0.image, Some("ubuntu:22.04".to_string()));
    }

    #[test]
    fn test_load_all_from_dir_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let results = DevContainerConfig::load_all_from_dir(tmp.path());
        assert!(results.is_empty());
    }

    #[test]
    fn test_load_all_from_dir_skips_invalid() {
        let tmp = tempfile::tempdir().unwrap();
        let dc = tmp.path().join(".devcontainer");
        std::fs::create_dir_all(dc.join("good")).unwrap();
        std::fs::create_dir_all(dc.join("bad")).unwrap();
        std::fs::write(
            dc.join("good/devcontainer.json"),
            r#"{"image": "ubuntu:22.04"}"#,
        )
        .unwrap();
        std::fs::write(
            dc.join("bad/devcontainer.json"),
            "not valid json {{{",
        )
        .unwrap();

        let results = DevContainerConfig::load_all_from_dir(tmp.path());
        assert_eq!(results.len(), 1);
        assert!(results[0].1.ends_with("good/devcontainer.json"));
    }
}
