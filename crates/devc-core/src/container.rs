//! Container configuration and operations

use crate::{CoreError, Result};
use devc_config::{DevContainerConfig, GlobalConfig, ImageSource, SubstitutionContext};
use devc_provider::{
    BuildConfig, ContainerId, ContainerProvider, CreateContainerConfig, ExecConfig,
    MountConfig, MountType, PortConfig,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Sanitize a name for CLI-friendly usage
/// - Converts to lowercase
/// - Replaces spaces and special chars with hyphens
/// - Collapses multiple hyphens
/// - Trims leading/trailing hyphens
fn sanitize_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();

    // Collapse multiple hyphens and trim
    let mut result = String::new();
    let mut last_was_hyphen = true; // Start true to skip leading hyphens
    for c in sanitized.chars() {
        if c == '-' {
            if !last_was_hyphen {
                result.push(c);
            }
            last_was_hyphen = true;
        } else {
            result.push(c);
            last_was_hyphen = false;
        }
    }

    // Trim trailing hyphen
    if result.ends_with('-') {
        result.pop();
    }

    if result.is_empty() {
        "container".to_string()
    } else {
        result
    }
}

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
        let (mut devcontainer, config_path) = DevContainerConfig::load_from_dir(workspace_path)?;
        let global_config = GlobalConfig::load()?;

        let container_workspace = devcontainer
            .workspace_folder
            .clone()
            .unwrap_or_else(|| "/workspace".to_string());
        let ctx = SubstitutionContext::new(
            workspace_path.to_string_lossy().to_string(),
            container_workspace,
        );
        devcontainer.substitute_variables(&ctx);

        let raw_name = devcontainer
            .name
            .clone()
            .or_else(|| {
                workspace_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "devcontainer".to_string());

        Ok(Self {
            name: sanitize_name(&raw_name),
            workspace_path: workspace_path.to_path_buf(),
            devcontainer,
            config_path,
            global_config,
        })
    }

    /// Load a container configuration from a specific devcontainer.json path
    pub fn from_config(config_path: &Path) -> Result<Self> {
        let mut devcontainer = DevContainerConfig::load_from(config_path)?;
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

        let container_workspace = devcontainer
            .workspace_folder
            .clone()
            .unwrap_or_else(|| "/workspace".to_string());
        let ctx = SubstitutionContext::new(
            workspace_path.to_string_lossy().to_string(),
            container_workspace,
        );
        devcontainer.substitute_variables(&ctx);

        let raw_name = devcontainer
            .name
            .clone()
            .or_else(|| {
                workspace_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
            })
            .unwrap_or_else(|| "devcontainer".to_string());

        Ok(Self {
            name: sanitize_name(&raw_name),
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

        // Add default environment variables for terminal support
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        // Enable 24-bit true color support (needed by nvim, tmux, etc.)
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        // Set UTF-8 locale for proper Unicode rendering (box-drawing chars, etc.)
        env.insert("LANG".to_string(), "C.UTF-8".to_string());
        env.insert("LC_ALL".to_string(), "C.UTF-8".to_string());

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

        // Determine CMD: if overrideCommand is false, use image default (None)
        let cmd = if self.devcontainer.override_command == Some(false) {
            None
        } else {
            Some(vec![
                self.global_config.defaults.shell.clone(),
                "-c".to_string(),
                "sleep infinity".to_string(),
            ])
        };

        CreateContainerConfig {
            image: image.to_string(),
            name: Some(self.container_name()),
            cmd,
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
            privileged: self.devcontainer.privileged.unwrap_or(false),
            cap_add: self.devcontainer.cap_add.clone().unwrap_or_default(),
            cap_drop: Vec::new(),
            security_opt: self.devcontainer.security_opt.clone().unwrap_or_default(),
            init: self.devcontainer.init.unwrap_or(false),
            extra_args: self.devcontainer.run_args.clone().unwrap_or_default(),
        }
    }

    /// Get exec configuration for running a command
    pub fn exec_config(&self, cmd: Vec<String>, tty: bool, stdin: bool) -> ExecConfig {
        let mut env = HashMap::new();
        if let Some(ref container_env) = self.devcontainer.container_env {
            env.extend(container_env.clone());
        }
        // Per spec: remoteEnv applies to tool processes (exec/shell), not container creation
        if let Some(ref remote_env) = self.devcontainer.remote_env {
            env.extend(remote_env.clone());
        }
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        // Enable 24-bit true color support (needed by nvim, tmux, etc.)
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        // Set UTF-8 locale for proper Unicode rendering (box-drawing chars, etc.)
        env.insert("LANG".to_string(), "C.UTF-8".to_string());
        env.insert("LC_ALL".to_string(), "C.UTF-8".to_string());

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

/// Run a lifecycle command on the host (for initializeCommand)
pub fn run_host_command(command: &devc_config::Command, working_dir: &Path) -> Result<()> {
    match command {
        devc_config::Command::String(cmd) => {
            let status = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(working_dir)
                .status()
                .map_err(|e| CoreError::ExecFailed(format!("Failed to run host command: {}", e)))?;
            if !status.success() {
                return Err(CoreError::ExecFailed(format!(
                    "Host command '{}' exited with code {}",
                    cmd,
                    status.code().unwrap_or(-1)
                )));
            }
        }
        devc_config::Command::Array(args) => {
            if args.is_empty() {
                return Ok(());
            }
            let status = std::process::Command::new(&args[0])
                .args(&args[1..])
                .current_dir(working_dir)
                .status()
                .map_err(|e| CoreError::ExecFailed(format!("Failed to run host command: {}", e)))?;
            if !status.success() {
                return Err(CoreError::ExecFailed(format!(
                    "Host command {:?} exited with code {}",
                    args,
                    status.code().unwrap_or(-1)
                )));
            }
        }
        devc_config::Command::Object(commands) => {
            for (name, cmd) in commands {
                tracing::info!("Running host command: {}", name);
                match cmd {
                    devc_config::StringOrArray::String(s) => {
                        run_host_command(
                            &devc_config::Command::String(s.clone()),
                            working_dir,
                        )?;
                    }
                    devc_config::StringOrArray::Array(args) => {
                        run_host_command(
                            &devc_config::Command::Array(args.clone()),
                            working_dir,
                        )?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Run lifecycle command(s) in a container
pub async fn run_lifecycle_command(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    command: &devc_config::Command,
    user: Option<&str>,
    working_dir: Option<&str>,
) -> Result<()> {
    run_lifecycle_command_with_env(provider, container_id, command, user, working_dir, None).await
}

/// Run lifecycle command(s) in a container with optional extra environment variables
pub async fn run_lifecycle_command_with_env(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    command: &devc_config::Command,
    user: Option<&str>,
    working_dir: Option<&str>,
    env: Option<&HashMap<String, String>>,
) -> Result<()> {
    let base_env = env.cloned().unwrap_or_default();

    match command {
        devc_config::Command::String(cmd) => {
            let config = ExecConfig {
                cmd: vec!["/bin/sh".to_string(), "-c".to_string(), cmd.clone()],
                env: base_env,
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
                env: base_env,
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
            // Run named commands concurrently
            use futures::future::try_join_all;

            let futures: Vec<_> = commands
                .iter()
                .map(|(name, cmd)| {
                    let name = name.clone();
                    let base_env = base_env.clone();
                    let working_dir = working_dir.map(|s| s.to_string());
                    let user = user.map(|s| s.to_string());
                    async move {
                        tracing::info!("Running lifecycle command: {}", name);
                        let config = match cmd {
                            devc_config::StringOrArray::String(s) => ExecConfig {
                                cmd: vec!["/bin/sh".to_string(), "-c".to_string(), s.clone()],
                                env: base_env,
                                working_dir,
                                user,
                                tty: false,
                                stdin: false,
                                privileged: false,
                            },
                            devc_config::StringOrArray::Array(args) => ExecConfig {
                                cmd: args.clone(),
                                env: base_env,
                                working_dir,
                                user,
                                tty: false,
                                stdin: false,
                                privileged: false,
                            },
                        };
                        let result = provider.exec(container_id, &config).await?;
                        if result.exit_code != 0 {
                            return Err(CoreError::ExecFailed(format!(
                                "Command '{}' exited with code {}",
                                name, result.exit_code
                            )));
                        }
                        Ok::<(), CoreError>(())
                    }
                })
                .collect();

            try_join_all(futures).await?;
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

    #[test]
    fn test_create_config_runtime_flags() {
        let config = DevContainerConfig {
            image: Some("ubuntu:22.04".to_string()),
            privileged: Some(true),
            cap_add: Some(vec!["SYS_PTRACE".to_string()]),
            security_opt: Some(vec!["seccomp=unconfined".to_string()]),
            init: Some(true),
            run_args: Some(vec!["--shm-size=1g".to_string()]),
            ..Default::default()
        };

        let container = Container {
            name: "test".to_string(),
            workspace_path: PathBuf::from("/tmp/test"),
            devcontainer: config,
            config_path: PathBuf::from("/tmp/test/.devcontainer/devcontainer.json"),
            global_config: GlobalConfig::default(),
        };

        let create = container.create_config("ubuntu:22.04");
        assert!(create.privileged);
        assert_eq!(create.cap_add, vec!["SYS_PTRACE"]);
        assert_eq!(create.security_opt, vec!["seccomp=unconfined"]);
        assert!(create.init);
        assert_eq!(create.extra_args, vec!["--shm-size=1g"]);
    }

    #[test]
    fn test_override_command_false() {
        let config = DevContainerConfig {
            image: Some("ubuntu:22.04".to_string()),
            override_command: Some(false),
            ..Default::default()
        };

        let container = Container {
            name: "test".to_string(),
            workspace_path: PathBuf::from("/tmp/test"),
            devcontainer: config,
            config_path: PathBuf::from("/tmp/test/.devcontainer/devcontainer.json"),
            global_config: GlobalConfig::default(),
        };

        let create = container.create_config("ubuntu:22.04");
        assert!(create.cmd.is_none(), "overrideCommand=false should yield cmd=None");
    }

    #[test]
    fn test_exec_config_includes_remote_env() {
        let config = DevContainerConfig {
            image: Some("ubuntu:22.04".to_string()),
            remote_env: Some({
                let mut m = HashMap::new();
                m.insert("EDITOR".to_string(), "vim".to_string());
                m
            }),
            ..Default::default()
        };

        let container = Container {
            name: "test".to_string(),
            workspace_path: PathBuf::from("/tmp/test"),
            devcontainer: config,
            config_path: PathBuf::from("/tmp/test/.devcontainer/devcontainer.json"),
            global_config: GlobalConfig::default(),
        };

        let exec = container.exec_config(vec!["echo".to_string()], false, false);
        assert_eq!(exec.env.get("EDITOR").unwrap(), "vim");
    }

    #[test]
    fn test_create_config_excludes_remote_env() {
        let config = DevContainerConfig {
            image: Some("ubuntu:22.04".to_string()),
            remote_env: Some({
                let mut m = HashMap::new();
                m.insert("EDITOR".to_string(), "vim".to_string());
                m
            }),
            ..Default::default()
        };

        let container = Container {
            name: "test".to_string(),
            workspace_path: PathBuf::from("/tmp/test"),
            devcontainer: config,
            config_path: PathBuf::from("/tmp/test/.devcontainer/devcontainer.json"),
            global_config: GlobalConfig::default(),
        };

        let create = container.create_config("ubuntu:22.04");
        // remoteEnv should NOT be in container creation env (per spec)
        assert!(!create.env.contains_key("EDITOR"));
    }

    // ==================== Additional sanitize_name tests ====================

    #[test]
    fn test_sanitize_name_all_special() {
        // All special characters should result in "container" fallback
        assert_eq!(sanitize_name("@#$%^&*"), "container");
    }

    #[test]
    fn test_sanitize_name_unicode() {
        // Unicode characters should be replaced with hyphens
        let result = sanitize_name("projekt-über");
        assert!(!result.contains("ü"));
        assert!(result.contains("projekt"));
    }

    // ==================== Additional parse_mount_string tests ====================

    #[test]
    fn test_parse_mount_string_volume() {
        let mount = parse_mount_string("type=volume,source=myvolume,target=/data");
        assert!(mount.is_some());
        let mount = mount.unwrap();
        assert!(matches!(mount.mount_type, MountType::Volume));
        assert_eq!(mount.source, "myvolume");
        assert_eq!(mount.target, "/data");
        assert!(!mount.read_only);
    }

    #[test]
    fn test_parse_mount_string_no_target() {
        // Missing target should return None
        let mount = parse_mount_string("type=bind,source=/host/path");
        assert!(mount.is_none());
    }

    // ==================== create_config default env vars ====================

    #[test]
    fn test_create_config_default_env_vars() {
        let config = DevContainerConfig {
            image: Some("ubuntu:22.04".to_string()),
            ..Default::default()
        };

        let container = Container {
            name: "test".to_string(),
            workspace_path: PathBuf::from("/tmp/test"),
            devcontainer: config,
            config_path: PathBuf::from("/tmp/test/.devcontainer/devcontainer.json"),
            global_config: GlobalConfig::default(),
        };

        let create = container.create_config("ubuntu:22.04");
        // Should have default terminal env vars
        assert_eq!(create.env.get("TERM").unwrap(), "xterm-256color");
        assert_eq!(create.env.get("COLORTERM").unwrap(), "truecolor");
        assert_eq!(create.env.get("LANG").unwrap(), "C.UTF-8");
        assert_eq!(create.env.get("LC_ALL").unwrap(), "C.UTF-8");
    }

    #[test]
    fn test_run_host_command_string() {
        let dir = std::env::temp_dir();
        let cmd = devc_config::Command::String("echo hello".to_string());
        let result = run_host_command(&cmd, &dir);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_host_command_array() {
        let dir = std::env::temp_dir();
        let cmd = devc_config::Command::Array(vec!["echo".to_string(), "hello".to_string()]);
        let result = run_host_command(&cmd, &dir);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_host_command_failure() {
        let dir = std::env::temp_dir();
        let cmd = devc_config::Command::String("false".to_string());
        let result = run_host_command(&cmd, &dir);
        assert!(result.is_err());
    }

    #[test]
    fn test_run_host_command_object() {
        let dir = std::env::temp_dir();
        let mut commands = HashMap::new();
        commands.insert("first".to_string(), devc_config::StringOrArray::String("echo one".to_string()));
        commands.insert("second".to_string(), devc_config::StringOrArray::String("echo two".to_string()));
        let cmd = devc_config::Command::Object(commands);
        let result = run_host_command(&cmd, &dir);
        assert!(result.is_ok());
    }
}
