//! Container configuration and operations

use crate::{CoreError, Result};
use devc_config::{DevContainerConfig, GlobalConfig, ImageSource};
use devc_provider::{
    BuildConfig, ContainerId, ContainerProvider, CreateContainerConfig, ExecConfig,
    MountConfig, MountType, PortConfig,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Represents a fully configured container ready for operations
#[derive(Debug, Clone)]
pub struct Container {
    /// Container name
    pub name: String,
    /// Path to workspace on host
    pub workspace_path: PathBuf,
    /// Parsed devcontainer configuration
    pub devcontainer: DevContainerConfig,
    /// Path to devcontainer.json
    pub config_path: PathBuf,
    /// Global configuration
    pub global_config: GlobalConfig,
}

impl Container {
    /// Load a container configuration from a workspace directory
    pub fn from_workspace(workspace_path: &Path) -> Result<Self> {
        let (devcontainer, config_path) = DevContainerConfig::load_from_dir(workspace_path)?;
        let global_config = GlobalConfig::load()?;

        let name = devcontainer
            .name
            .clone()
            .or_else(|| {
                workspace_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "devcontainer".to_string());

        Ok(Self {
            name,
            workspace_path: workspace_path.to_path_buf(),
            devcontainer,
            config_path,
            global_config,
        })
    }

    /// Load a container configuration from a specific devcontainer.json path
    pub fn from_config(config_path: &Path) -> Result<Self> {
        let devcontainer = DevContainerConfig::load_from(config_path)?;
        let global_config = GlobalConfig::load()?;

        // Workspace is parent of .devcontainer directory or config file
        let workspace_path = config_path
            .parent()
            .and_then(|p| {
                if p.file_name().map(|n| n == ".devcontainer").unwrap_or(false) {
                    p.parent()
                } else {
                    Some(p)
                }
            })
            .unwrap_or(Path::new("."))
            .to_path_buf();

        let name = devcontainer
            .name
            .clone()
            .or_else(|| {
                workspace_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "devcontainer".to_string());

        Ok(Self {
            name,
            workspace_path,
            devcontainer,
            config_path: config_path.to_path_buf(),
            global_config,
        })
    }

    /// Generate a unique container name for Docker/Podman
    pub fn container_name(&self) -> String {
        // Sanitize the name for Docker (must be lowercase)
        let sanitized: String = self
            .name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        format!("devc_{}", sanitized)
    }

    /// Generate the image tag
    pub fn image_tag(&self) -> String {
        // Docker image tags must be lowercase
        format!("devc/{}:latest", self.container_name())
    }

    /// Get the build configuration
    pub fn build_config(&self) -> Result<BuildConfig> {
        let context = self
            .config_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();

        let (dockerfile, build_args, target) = match self.devcontainer.image_source() {
            ImageSource::Image(image) => {
                // No build needed for pre-built images
                return Err(CoreError::InvalidState(format!(
                    "Cannot build pre-built image: {}",
                    image
                )));
            }
            ImageSource::Dockerfile { path, args, .. } => {
                (path, args.unwrap_or_default(), None)
            }
            ImageSource::Compose => {
                return Err(CoreError::InvalidState(
                    "Docker Compose not yet supported".to_string(),
                ));
            }
            ImageSource::None => {
                return Err(CoreError::InvalidState(
                    "No image source specified in devcontainer.json".to_string(),
                ));
            }
        };

        let mut labels = HashMap::new();
        labels.insert("devc.managed".to_string(), "true".to_string());
        labels.insert("devc.project".to_string(), self.name.clone());

        Ok(BuildConfig {
            context,
            dockerfile,
            tag: self.image_tag(),
            build_args,
            target,
            cache_from: Vec::new(),
            labels,
            no_cache: false,
            pull: true,
        })
    }

    /// Get the container creation configuration
    pub fn create_config(&self, image: &str) -> CreateContainerConfig {
        let _workspace_mount = format!(
            "{}:/workspace",
            self.workspace_path.to_string_lossy()
        );

        let mut mounts = vec![MountConfig {
            mount_type: MountType::Bind,
            source: self.workspace_path.to_string_lossy().to_string(),
            target: self
                .devcontainer
                .workspace_folder
                .clone()
                .unwrap_or_else(|| "/workspace".to_string()),
            read_only: false,
        }];

        // Add configured mounts
        if let Some(ref configured_mounts) = self.devcontainer.mounts {
            for mount in configured_mounts {
                match mount {
                    devc_config::Mount::String(s) => {
                        if let Some(config) = parse_mount_string(s) {
                            mounts.push(config);
                        }
                    }
                    devc_config::Mount::Object(obj) => {
                        let mount_type = match obj.mount_type.as_deref() {
                            Some("volume") => MountType::Volume,
                            Some("tmpfs") => MountType::Tmpfs,
                            _ => MountType::Bind,
                        };
                        mounts.push(MountConfig {
                            mount_type,
                            source: obj.source.clone().unwrap_or_default(),
                            target: obj.target.clone(),
                            read_only: obj.read_only.unwrap_or(false),
                        });
                    }
                }
            }
        }

        // Build port mappings
        let mut ports = Vec::new();
        for port in self.devcontainer.forward_ports_list() {
            ports.push(PortConfig {
                host_port: Some(port),
                container_port: port,
                protocol: "tcp".to_string(),
                host_ip: Some("127.0.0.1".to_string()),
            });
        }

        // Build environment variables
        let mut env = HashMap::new();
        if let Some(ref container_env) = self.devcontainer.container_env {
            env.extend(container_env.clone());
        }

        // Add default environment variables
        env.insert("TERM".to_string(), "xterm-256color".to_string());

        // Build labels
        let mut labels = HashMap::new();
        labels.insert("devc.managed".to_string(), "true".to_string());
        labels.insert("devc.project".to_string(), self.name.clone());
        labels.insert(
            "devc.workspace".to_string(),
            self.workspace_path.to_string_lossy().to_string(),
        );
        labels.insert(
            "devc.config".to_string(),
            self.config_path.to_string_lossy().to_string(),
        );

        // Get user
        let user = self
            .devcontainer
            .effective_user()
            .map(|s| s.to_string())
            .or_else(|| self.global_config.defaults.user.clone());

        // Get working directory
        let working_dir = self
            .devcontainer
            .workspace_folder
            .clone()
            .or_else(|| Some("/workspace".to_string()));

        CreateContainerConfig {
            image: image.to_string(),
            name: Some(self.container_name()),
            cmd: Some(vec![
                self.global_config.defaults.shell.clone(),
                "-c".to_string(),
                "sleep infinity".to_string(),
            ]),
            entrypoint: None,
            env,
            working_dir,
            user,
            mounts,
            ports,
            labels,
            hostname: Some(self.name.clone()),
            tty: true,
            stdin_open: true,
            network_mode: None,
            privileged: false,
            cap_add: Vec::new(),
            cap_drop: Vec::new(),
            security_opt: Vec::new(),
        }
    }

    /// Get exec configuration for running a command
    pub fn exec_config(&self, cmd: Vec<String>, tty: bool, stdin: bool) -> ExecConfig {
        let mut env = HashMap::new();
        if let Some(ref container_env) = self.devcontainer.container_env {
            env.extend(container_env.clone());
        }
        env.insert("TERM".to_string(), "xterm-256color".to_string());

        ExecConfig {
            cmd,
            env,
            working_dir: self.devcontainer.workspace_folder.clone(),
            user: self.devcontainer.effective_user().map(|s| s.to_string()),
            tty,
            stdin,
            privileged: false,
        }
    }

    /// Get shell exec configuration
    pub fn shell_config(&self) -> ExecConfig {
        let shell = self.global_config.defaults.shell.clone();
        self.exec_config(vec![shell], true, true)
    }
}

/// Parse a mount string like "type=bind,source=/path,target=/path"
fn parse_mount_string(s: &str) -> Option<MountConfig> {
    let mut mount_type = MountType::Bind;
    let mut source = String::new();
    let mut target = String::new();
    let mut read_only = false;

    for part in s.split(',') {
        let parts: Vec<&str> = part.splitn(2, '=').collect();
        if parts.len() != 2 {
            continue;
        }

        match parts[0] {
            "type" => {
                mount_type = match parts[1] {
                    "volume" => MountType::Volume,
                    "tmpfs" => MountType::Tmpfs,
                    _ => MountType::Bind,
                };
            }
            "source" | "src" => source = parts[1].to_string(),
            "target" | "dst" | "destination" => target = parts[1].to_string(),
            "readonly" | "ro" => read_only = parts[1] == "true" || parts[1] == "1",
            _ => {}
        }
    }

    if target.is_empty() {
        return None;
    }

    Some(MountConfig {
        mount_type,
        source,
        target,
        read_only,
    })
}

/// Run lifecycle command(s) in a container
pub async fn run_lifecycle_command(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    command: &devc_config::Command,
    user: Option<&str>,
    working_dir: Option<&str>,
) -> Result<()> {
    match command {
        devc_config::Command::String(cmd) => {
            let config = ExecConfig {
                cmd: vec!["/bin/sh".to_string(), "-c".to_string(), cmd.clone()],
                env: HashMap::new(),
                working_dir: working_dir.map(|s| s.to_string()),
                user: user.map(|s| s.to_string()),
                tty: false,
                stdin: false,
                privileged: false,
            };

            let result = provider.exec(container_id, &config).await?;
            if result.exit_code != 0 {
                return Err(CoreError::ExecFailed(format!(
                    "Command '{}' exited with code {}",
                    cmd, result.exit_code
                )));
            }
        }
        devc_config::Command::Array(args) => {
            let config = ExecConfig {
                cmd: args.clone(),
                env: HashMap::new(),
                working_dir: working_dir.map(|s| s.to_string()),
                user: user.map(|s| s.to_string()),
                tty: false,
                stdin: false,
                privileged: false,
            };

            let result = provider.exec(container_id, &config).await?;
            if result.exit_code != 0 {
                return Err(CoreError::ExecFailed(format!(
                    "Command {:?} exited with code {}",
                    args, result.exit_code
                )));
            }
        }
        devc_config::Command::Object(commands) => {
            // Run commands in parallel (not truly parallel here, but sequentially for simplicity)
            // TODO: Use tokio::join! for true parallelism
            for (name, cmd) in commands {
                tracing::info!("Running lifecycle command: {}", name);
                match cmd {
                    devc_config::StringOrArray::String(s) => {
                        let config = ExecConfig {
                            cmd: vec!["/bin/sh".to_string(), "-c".to_string(), s.clone()],
                            env: HashMap::new(),
                            working_dir: working_dir.map(|s| s.to_string()),
                            user: user.map(|s| s.to_string()),
                            tty: false,
                            stdin: false,
                            privileged: false,
                        };

                        let result = provider.exec(container_id, &config).await?;
                        if result.exit_code != 0 {
                            return Err(CoreError::ExecFailed(format!(
                                "Command '{}' ({}) exited with code {}",
                                name, s, result.exit_code
                            )));
                        }
                    }
                    devc_config::StringOrArray::Array(args) => {
                        let config = ExecConfig {
                            cmd: args.clone(),
                            env: HashMap::new(),
                            working_dir: working_dir.map(|s| s.to_string()),
                            user: user.map(|s| s.to_string()),
                            tty: false,
                            stdin: false,
                            privileged: false,
                        };

                        let result = provider.exec(container_id, &config).await?;
                        if result.exit_code != 0 {
                            return Err(CoreError::ExecFailed(format!(
                                "Command '{}' ({:?}) exited with code {}",
                                name, args, result.exit_code
                            )));
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_mount_string() {
        let mount = parse_mount_string("type=bind,source=/host/path,target=/container/path,readonly=true");
        assert!(mount.is_some());
        let mount = mount.unwrap();
        assert!(matches!(mount.mount_type, MountType::Bind));
        assert_eq!(mount.source, "/host/path");
        assert_eq!(mount.target, "/container/path");
        assert!(mount.read_only);
    }

    #[test]
    fn test_container_name_sanitization() {
        // Create a mock container with special characters in name
        let config = DevContainerConfig {
            name: Some("My Project!@#$%".to_string()),
            ..Default::default()
        };

        let container = Container {
            name: config.name.clone().unwrap(),
            workspace_path: PathBuf::from("/tmp/test"),
            devcontainer: config,
            config_path: PathBuf::from("/tmp/test/.devcontainer/devcontainer.json"),
            global_config: GlobalConfig::default(),
        };

        let name = container.container_name();
        // Must be valid Docker name: lowercase alphanumeric, hyphen, underscore
        assert!(name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'));
        assert_eq!(name, "devc_my_project_____");
    }
}
