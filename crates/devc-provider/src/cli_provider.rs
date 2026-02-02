//! CLI-based container provider for Docker and Podman
//!
//! Uses direct CLI commands instead of API for:
//! - Simpler implementation
//! - Automatic credential handling (via ~/.docker/config.json)
//! - Proper user context handling (no permissions issues)
//! - Works with Docker alternatives (Colima, Rancher, Lima, OrbStack)

use crate::{
    BuildConfig, ContainerDetails, ContainerId, ContainerInfo, ContainerProvider, ContainerStatus,
    CreateContainerConfig, DevcontainerSource, DiscoveredContainer, ExecConfig, ExecResult,
    ExecStream, ImageId, LogConfig, LogStream, MountInfo, MountType, NetworkInfo, NetworkSettings,
    PortInfo, ProviderError, ProviderInfo, ProviderType, Result,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// CLI-based container provider for Docker and Podman
pub struct CliProvider {
    /// Command to use ("docker" or "podman")
    cmd: String,
    /// Optional prefix (e.g., ["flatpak-spawn", "--host"] for Toolbox)
    cmd_prefix: Vec<String>,
    /// Provider type
    provider_type: ProviderType,
}

impl CliProvider {
    /// Create a new Docker provider
    pub async fn new_docker() -> Result<Self> {
        let provider = Self {
            cmd: "docker".to_string(),
            cmd_prefix: Vec::new(),
            provider_type: ProviderType::Docker,
        };

        // Test connection
        provider.ping().await?;
        Ok(provider)
    }

    /// Create a new Podman provider
    pub async fn new_podman() -> Result<Self> {
        let provider = Self {
            cmd: "podman".to_string(),
            cmd_prefix: Vec::new(),
            provider_type: ProviderType::Podman,
        };

        // Test connection
        provider.ping().await?;
        Ok(provider)
    }

    /// Create a new provider for Toolbox environment (flatpak-spawn --host podman)
    pub async fn new_toolbox() -> Result<Self> {
        let provider = Self {
            cmd: "podman".to_string(),
            cmd_prefix: vec!["flatpak-spawn".to_string(), "--host".to_string()],
            provider_type: ProviderType::Podman,
        };

        // Test connection
        provider.ping().await?;
        Ok(provider)
    }

    /// Run a command and get output
    async fn run_cmd(&self, args: &[&str]) -> Result<String> {
        let mut cmd = if self.cmd_prefix.is_empty() {
            Command::new(&self.cmd)
        } else {
            let mut c = Command::new(&self.cmd_prefix[0]);
            for prefix_arg in &self.cmd_prefix[1..] {
                c.arg(prefix_arg);
            }
            c.arg(&self.cmd);
            c
        };
        cmd.args(args);

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| ProviderError::RuntimeError(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProviderError::RuntimeError(stderr.to_string()));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Build a command with the correct prefix
    fn build_command(&self) -> Command {
        if self.cmd_prefix.is_empty() {
            Command::new(&self.cmd)
        } else {
            let mut c = Command::new(&self.cmd_prefix[0]);
            for prefix_arg in &self.cmd_prefix[1..] {
                c.arg(prefix_arg);
            }
            c.arg(&self.cmd);
            c
        }
    }

    /// Check if we should use --userns=keep-id (podman rootless)
    fn use_keep_id(&self) -> bool {
        self.provider_type == ProviderType::Podman
    }

    /// Get SELinux mount option for bind mounts
    fn selinux_mount_opt(&self) -> &'static str {
        // Use :Z for SELinux relabeling on bind mounts (required on Fedora/RHEL)
        if self.provider_type == ProviderType::Podman {
            ":Z"
        } else {
            ""
        }
    }
}

#[async_trait]
impl ContainerProvider for CliProvider {
    async fn build(&self, config: &BuildConfig) -> Result<ImageId> {
        let context = config.context.to_string_lossy();
        // Use absolute path for Dockerfile to ensure BuildKit finds it correctly
        let dockerfile_path = config.context.join(&config.dockerfile);
        let dockerfile = format!("-f={}", dockerfile_path.display());
        let tag = format!("-t={}", config.tag);

        let mut args = vec!["build", &dockerfile, &tag];

        if config.no_cache {
            args.push("--no-cache");
        }

        if config.pull {
            args.push("--pull");
        }

        // Add build args
        let build_args: Vec<String> = config
            .build_args
            .iter()
            .map(|(k, v)| format!("--build-arg={}={}", k, v))
            .collect();
        for arg in &build_args {
            args.push(arg);
        }

        // Add labels
        let labels: Vec<String> = config
            .labels
            .iter()
            .map(|(k, v)| format!("--label={}={}", k, v))
            .collect();
        for label in &labels {
            args.push(label);
        }

        args.push(&context);

        let output = self.run_cmd(&args).await?;
        tracing::debug!("Build output: {}", output);

        // Get the image ID
        let inspect_output = self
            .run_cmd(&["inspect", "--format={{.Id}}", &config.tag])
            .await?;
        Ok(ImageId::new(inspect_output.trim()))
    }

    async fn build_with_progress(
        &self,
        config: &BuildConfig,
        progress: mpsc::UnboundedSender<String>,
    ) -> Result<ImageId> {
        let context = config.context.to_string_lossy();
        // Use absolute path for Dockerfile to ensure BuildKit finds it correctly
        let dockerfile_path = config.context.join(&config.dockerfile);
        let dockerfile = format!("-f={}", dockerfile_path.display());
        let tag = format!("-t={}", config.tag);

        let mut args = vec!["build".to_string(), dockerfile, tag];

        if config.no_cache {
            args.push("--no-cache".to_string());
            tracing::debug!("Build using --no-cache flag");
        }

        if config.pull {
            args.push("--pull".to_string());
        }

        // Add build args
        for (k, v) in &config.build_args {
            args.push(format!("--build-arg={}={}", k, v));
        }

        // Add labels
        for (k, v) in &config.labels {
            args.push(format!("--label={}={}", k, v));
        }

        args.push(context.to_string());

        // Spawn the build command with streaming output
        let mut cmd = self.build_command();
        for arg in &args {
            cmd.arg(arg);
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| ProviderError::RuntimeError(e.to_string()))?;

        // Stream stderr (where build output goes for podman/docker build)
        if let Some(stderr) = child.stderr.take() {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = progress.send(line);
            }
        }

        // Wait for completion
        let status = child
            .wait()
            .await
            .map_err(|e| ProviderError::RuntimeError(e.to_string()))?;

        if !status.success() {
            let _ = progress.send("Build failed".to_string());
            return Err(ProviderError::BuildError("Build failed".to_string()));
        }

        // Get the image ID
        let inspect_output = self
            .run_cmd(&["inspect", "--format={{.Id}}", &config.tag])
            .await?;
        Ok(ImageId::new(inspect_output.trim()))
    }

    async fn pull(&self, image: &str) -> Result<ImageId> {
        self.run_cmd(&["pull", image]).await?;

        let output = self
            .run_cmd(&["inspect", "--format={{.Id}}", image])
            .await?;
        Ok(ImageId::new(output.trim()))
    }

    async fn create(&self, config: &CreateContainerConfig) -> Result<ContainerId> {
        let mut args = vec!["create".to_string()];

        // Use keep-id to map host user into container for proper file permissions
        // This is essential for rootless podman with bind mounts
        if self.use_keep_id() {
            args.push("--userns=keep-id".to_string());
        }

        // Name
        if let Some(ref name) = config.name {
            args.push(format!("--name={}", name));
        }

        // TTY and stdin
        if config.tty {
            args.push("-t".to_string());
        }
        if config.stdin_open {
            args.push("-i".to_string());
        }

        // Environment
        for (k, v) in &config.env {
            args.push(format!("--env={}={}", k, v));
        }

        // Working directory
        if let Some(ref wd) = config.working_dir {
            args.push(format!("--workdir={}", wd));
        }

        // User
        if let Some(ref user) = config.user {
            args.push(format!("--user={}", user));
        }

        // Mounts
        let selinux_opt = self.selinux_mount_opt();
        for mount in &config.mounts {
            let mount_str = match mount.mount_type {
                MountType::Bind => {
                    let ro = if mount.read_only { ":ro" } else { "" };
                    format!("-v={}:{}{}{}", mount.source, mount.target, selinux_opt, ro)
                }
                MountType::Volume => format!(
                    "--mount=type=volume,source={},target={}",
                    mount.source, mount.target
                ),
                MountType::Tmpfs => format!("--mount=type=tmpfs,target={}", mount.target),
            };
            args.push(mount_str);
        }

        // Ports
        for port in &config.ports {
            let port_str = match (port.host_port, &port.host_ip) {
                (Some(hp), Some(ip)) => format!("-p={}:{}:{}", ip, hp, port.container_port),
                (Some(hp), None) => format!("-p={}:{}", hp, port.container_port),
                (None, _) => format!("-p={}", port.container_port),
            };
            args.push(port_str);
        }

        // Labels
        for (k, v) in &config.labels {
            args.push(format!("--label={}={}", k, v));
        }

        // Network mode
        if let Some(ref network) = config.network_mode {
            args.push(format!("--network={}", network));
        }

        // Privileged
        if config.privileged {
            args.push("--privileged".to_string());
        }

        // Capabilities
        for cap in &config.cap_add {
            args.push(format!("--cap-add={}", cap));
        }
        for cap in &config.cap_drop {
            args.push(format!("--cap-drop={}", cap));
        }

        // Security options
        for opt in &config.security_opt {
            args.push(format!("--security-opt={}", opt));
        }

        // Image
        args.push(config.image.clone());

        // Command
        if let Some(ref cmd) = config.cmd {
            args.extend(cmd.clone());
        }

        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let output = self.run_cmd(&args_refs).await?;

        Ok(ContainerId::new(output.trim()))
    }

    async fn start(&self, id: &ContainerId) -> Result<()> {
        self.run_cmd(&["start", &id.0]).await?;
        Ok(())
    }

    async fn stop(&self, id: &ContainerId, timeout: Option<u32>) -> Result<()> {
        let timeout_str = timeout.unwrap_or(10).to_string();
        self.run_cmd(&["stop", "-t", &timeout_str, &id.0]).await?;
        Ok(())
    }

    async fn remove(&self, id: &ContainerId, force: bool) -> Result<()> {
        if force {
            self.run_cmd(&["rm", "-f", &id.0]).await?;
        } else {
            self.run_cmd(&["rm", &id.0]).await?;
        }
        Ok(())
    }

    async fn remove_by_name(&self, name: &str) -> Result<()> {
        // Best effort removal - ignore errors since container may not exist
        tracing::debug!("Removing container by name (if exists): {}", name);
        let _ = self.run_cmd(&["rm", "-f", name]).await;
        Ok(())
    }

    async fn exec(&self, id: &ContainerId, config: &ExecConfig) -> Result<ExecResult> {
        let mut args = vec!["exec".to_string()];

        if config.tty {
            args.push("-t".to_string());
        }

        if let Some(ref wd) = config.working_dir {
            args.push(format!("--workdir={}", wd));
        }

        if let Some(ref user) = config.user {
            args.push(format!("--user={}", user));
        }

        for (k, v) in &config.env {
            args.push(format!("--env={}={}", k, v));
        }

        args.push(id.0.clone());
        args.extend(config.cmd.clone());

        let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();

        let mut cmd = self.build_command();
        cmd.args(&args_refs);

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| ProviderError::ExecError(e.to_string()))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        Ok(ExecResult {
            exit_code: output.status.code().unwrap_or(-1) as i64,
            output: format!("{}{}", stdout, stderr),
        })
    }

    async fn exec_interactive(&self, id: &ContainerId, config: &ExecConfig) -> Result<ExecStream> {
        // For interactive exec, we need to spawn a process with stdin/stdout
        let mut args = vec!["exec".to_string(), "-i".to_string()];

        if config.tty {
            args.push("-t".to_string());
        }

        if let Some(ref wd) = config.working_dir {
            args.push(format!("--workdir={}", wd));
        }

        args.push(id.0.clone());
        args.extend(config.cmd.clone());

        let mut cmd = self.build_command();
        cmd.args(&args[..]);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| ProviderError::ExecError(e.to_string()))?;

        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();

        Ok(ExecStream {
            stdin: stdin.map(|s| Box::pin(s) as Pin<Box<dyn tokio::io::AsyncWrite + Send>>),
            output: Box::pin(stdout),
            id: id.0.clone(),
        })
    }

    async fn list(&self, all: bool) -> Result<Vec<ContainerInfo>> {
        let filter = "--filter=label=devc.managed=true";
        let format = "--format={{.ID}}|{{.Names}}|{{.Image}}|{{.State}}|{{.Created}}";

        let args = if all {
            vec!["ps", "-a", filter, format]
        } else {
            vec!["ps", filter, format]
        };

        let output = self.run_cmd(&args).await?;

        let mut containers = Vec::new();
        for line in output.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() >= 4 {
                containers.push(ContainerInfo {
                    id: ContainerId::new(parts[0]),
                    name: parts[1].to_string(),
                    image: parts[2].to_string(),
                    status: ContainerStatus::from(parts[3]),
                    created: 0, // Would need to parse
                    labels: HashMap::new(),
                });
            }
        }

        Ok(containers)
    }

    async fn inspect(&self, id: &ContainerId) -> Result<ContainerDetails> {
        let output = self.run_cmd(&["inspect", "--format=json", &id.0]).await?;

        let inspect: Vec<serde_json::Value> = serde_json::from_str(&output)
            .map_err(|e: serde_json::Error| ProviderError::RuntimeError(e.to_string()))?;

        let info = inspect
            .first()
            .ok_or_else(|| ProviderError::ContainerNotFound(id.0.clone()))?;

        let state = info.get("State").and_then(serde_json::Value::as_object);
        let config = info.get("Config").and_then(serde_json::Value::as_object);
        let _host_config = info.get("HostConfig").and_then(serde_json::Value::as_object);

        let status = state
            .and_then(|s| s.get("Status"))
            .and_then(serde_json::Value::as_str)
            .map(ContainerStatus::from)
            .unwrap_or(ContainerStatus::Unknown);

        let name = info
            .get("Name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim_start_matches('/')
            .to_string();

        let image = config
            .and_then(|c| c.get("Image"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();

        let image_id = info
            .get("Image")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();

        let exit_code = state
            .and_then(|s| s.get("ExitCode"))
            .and_then(serde_json::Value::as_i64);

        let labels: HashMap<String, String> = config
            .and_then(|c| c.get("Labels"))
            .and_then(serde_json::Value::as_object)
            .map(|l| {
                l.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        // Parse environment variables
        let env: Vec<String> = config
            .and_then(|c| c.get("Env"))
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        // Parse mounts
        let mounts: Vec<MountInfo> = info
            .get("Mounts")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .map(|m| {
                        let mount_type = m.get("Type").and_then(|v| v.as_str()).unwrap_or("bind");
                        let source = m
                            .get("Source")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let destination = m
                            .get("Destination")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let rw = m.get("RW").and_then(|v| v.as_bool()).unwrap_or(true);

                        MountInfo {
                            mount_type: mount_type.to_string(),
                            source,
                            destination,
                            read_only: !rw,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Parse ports from NetworkSettings
        let network_settings_json = info.get("NetworkSettings");
        let mut ports: Vec<PortInfo> = Vec::new();

        if let Some(ns) = network_settings_json.and_then(|n| n.as_object()) {
            if let Some(port_map) = ns.get("Ports").and_then(|p| p.as_object()) {
                for (container_port_str, bindings) in port_map {
                    // Parse "80/tcp" format
                    let parts: Vec<&str> = container_port_str.split('/').collect();
                    let port_num: u16 = parts[0].parse().unwrap_or(0);
                    let protocol = parts.get(1).unwrap_or(&"tcp").to_string();

                    if let Some(binding_array) = bindings.as_array() {
                        for binding in binding_array {
                            let host_ip = binding
                                .get("HostIp")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());
                            let host_port = binding
                                .get("HostPort")
                                .and_then(|v| v.as_str())
                                .and_then(|s| s.parse().ok());

                            ports.push(PortInfo {
                                container_port: port_num,
                                host_port,
                                protocol: protocol.clone(),
                                host_ip,
                            });
                        }
                    } else if !bindings.is_null() {
                        // No bindings
                        ports.push(PortInfo {
                            container_port: port_num,
                            host_port: None,
                            protocol,
                            host_ip: None,
                        });
                    }
                }
            }
        }

        // Parse network settings
        let network_settings = network_settings_json
            .and_then(|ns| ns.as_object())
            .map(|ns| {
                let ip_address = ns.get("IPAddress").and_then(|v| v.as_str()).map(|s| s.to_string());
                let gateway = ns.get("Gateway").and_then(|v| v.as_str()).map(|s| s.to_string());

                let networks = ns
                    .get("Networks")
                    .and_then(|n| n.as_object())
                    .map(|nets| {
                        nets.iter()
                            .map(|(name, net)| {
                                let network_id = net
                                    .get("NetworkID")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let net_ip = net
                                    .get("IPAddress")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());
                                let net_gateway = net
                                    .get("Gateway")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string());

                                (
                                    name.clone(),
                                    NetworkInfo {
                                        network_id,
                                        ip_address: net_ip,
                                        gateway: net_gateway,
                                    },
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                NetworkSettings {
                    ip_address,
                    gateway,
                    networks,
                }
            })
            .unwrap_or_default();

        // Parse timestamps
        let created = info
            .get("Created")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp())
            .unwrap_or(0);

        let started_at = state
            .and_then(|s| s.get("StartedAt"))
            .and_then(serde_json::Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp());

        let finished_at = state
            .and_then(|s| s.get("FinishedAt"))
            .and_then(serde_json::Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp());

        Ok(ContainerDetails {
            id: id.clone(),
            name,
            image,
            image_id,
            status,
            created,
            started_at,
            finished_at,
            exit_code,
            labels,
            env,
            mounts,
            ports,
            network_settings,
        })
    }

    async fn logs(&self, id: &ContainerId, config: &LogConfig) -> Result<LogStream> {
        let mut args = vec!["logs".to_string()];

        if config.follow {
            args.push("-f".to_string());
        }
        if config.timestamps {
            args.push("-t".to_string());
        }
        if let Some(tail) = config.tail {
            args.push(format!("--tail={}", tail));
        }

        args.push(id.0.clone());

        let mut cmd = self.build_command();
        cmd.args(&args[..]);
        cmd.stdout(Stdio::piped());

        let child = cmd
            .spawn()
            .map_err(|e| ProviderError::RuntimeError(e.to_string()))?;

        Ok(LogStream {
            stream: Box::pin(child.stdout.unwrap()),
        })
    }

    async fn ping(&self) -> Result<()> {
        self.run_cmd(&["--version"]).await?;
        Ok(())
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            provider_type: self.provider_type,
            version: "cli".to_string(),
            api_version: "cli".to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        }
    }

    async fn copy_into(&self, id: &ContainerId, src: &Path, dest: &str) -> Result<()> {
        // Append /. to copy directory contents instead of the directory itself
        // This ensures /path/to/dotfiles/. -> container:/home/user/.dotfiles
        // copies the contents of dotfiles INTO .dotfiles, not as a subdirectory
        let src_str = format!("{}{}.", src.to_string_lossy(), std::path::MAIN_SEPARATOR);
        let target = format!("{}:{}", id.0, dest);
        self.run_cmd(&["cp", &src_str, &target]).await?;
        Ok(())
    }

    async fn copy_from(&self, id: &ContainerId, src: &str, dest: &Path) -> Result<()> {
        let source = format!("{}:{}", id.0, src);
        let dest_str = dest.to_string_lossy();
        self.run_cmd(&["cp", &source, &dest_str]).await?;
        Ok(())
    }

    async fn discover_devcontainers(&self) -> Result<Vec<DiscoveredContainer>> {
        // List ALL containers with detailed format including labels
        let format = "--format={{.ID}}|{{.Names}}|{{.Image}}|{{.State}}|{{.Labels}}";
        let output = self.run_cmd(&["ps", "-a", format]).await?;

        let mut discovered = Vec::new();
        for line in output.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() < 5 {
                continue;
            }

            let labels = parse_cli_labels(parts[4]);
            let (is_devcontainer, source, managed) = detect_devcontainer_source_from_labels(&labels);

            if !is_devcontainer {
                continue;
            }

            // Extract workspace path from labels
            let workspace_path = labels.get("devcontainer.local_folder").cloned();

            discovered.push(DiscoveredContainer {
                id: ContainerId::new(parts[0]),
                name: parts[1].to_string(),
                image: parts[2].to_string(),
                status: ContainerStatus::from(parts[3]),
                managed,
                source,
                workspace_path,
                labels,
            });
        }

        Ok(discovered)
    }
}

/// Parse CLI labels format "key=value,key2=value2" into HashMap
fn parse_cli_labels(label_str: &str) -> HashMap<String, String> {
    let mut labels = HashMap::new();
    for part in label_str.split(',') {
        if let Some((key, value)) = part.split_once('=') {
            labels.insert(key.to_string(), value.to_string());
        }
    }
    labels
}

/// Detect the source of a devcontainer based on labels
fn detect_devcontainer_source_from_labels(
    labels: &HashMap<String, String>,
) -> (bool, DevcontainerSource, bool) {
    // Check for devc-managed container
    if labels.contains_key("devc.managed") {
        return (true, DevcontainerSource::Devc, true);
    }

    // Check for VS Code devcontainer labels
    if labels.contains_key("devcontainer.local_folder")
        || labels.contains_key("devcontainer.config_file")
        || labels
            .get("vscode.devcontainer")
            .map(|v| v == "true")
            .unwrap_or(false)
    {
        return (true, DevcontainerSource::VsCode, false);
    }

    // Check for common devcontainer patterns
    if labels.keys().any(|k| k.starts_with("devcontainer.")) {
        return (true, DevcontainerSource::Other, false);
    }

    (false, DevcontainerSource::Other, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Get a provider for testing (tries toolbox, podman, then docker)
    async fn get_test_provider() -> Option<CliProvider> {
        // Try toolbox first (for Fedora toolbox environment)
        if let Ok(p) = CliProvider::new_toolbox().await {
            return Some(p);
        }
        // Try podman
        if let Ok(p) = CliProvider::new_podman().await {
            return Some(p);
        }
        // Fall back to docker
        if let Ok(p) = CliProvider::new_docker().await {
            return Some(p);
        }
        None
    }

    #[tokio::test]
    async fn test_container_name_conflict_cleanup() {
        let provider = match get_test_provider().await {
            Some(p) => p,
            None => {
                eprintln!("Skipping test: no container runtime available");
                return;
            }
        };

        let test_name = "devc_test_orphan_cleanup";

        // Create a "orphaned" container
        let _ = provider
            .run_cmd(&["run", "-d", "--name", test_name, "alpine", "sleep", "1"])
            .await;

        // Wait for it to exit
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Verify it exists (exited state)
        let list = provider
            .run_cmd(&[
                "ps",
                "-a",
                "--filter",
                &format!("name={}", test_name),
                "--format",
                "{{.Names}}",
            ])
            .await;
        assert!(list.is_ok());
        assert!(
            list.unwrap().contains(test_name),
            "Orphaned container should exist"
        );

        // Now remove by name (the fix we're testing)
        let result = provider.remove_by_name(test_name).await;
        assert!(result.is_ok(), "remove_by_name should succeed");

        // Verify it's gone
        let list = provider
            .run_cmd(&[
                "ps",
                "-a",
                "--filter",
                &format!("name={}", test_name),
                "--format",
                "{{.Names}}",
            ])
            .await;
        assert!(list.is_ok());
        assert!(
            !list.unwrap().contains(test_name),
            "Container should be removed"
        );
    }

    #[tokio::test]
    async fn test_build_no_cache_flag() {
        let provider = match get_test_provider().await {
            Some(p) => p,
            None => {
                eprintln!("Skipping test: no container runtime available");
                return;
            }
        };

        let temp = tempdir().unwrap();
        let dockerfile = "FROM alpine:latest\nRUN echo test\n";
        fs::write(temp.path().join("Dockerfile"), dockerfile).unwrap();

        let config = BuildConfig {
            context: temp.path().to_path_buf(),
            dockerfile: "Dockerfile".to_string(),
            tag: "devc-test-nocache:latest".to_string(),
            no_cache: true, // Test this flag
            ..Default::default()
        };

        // Build should succeed
        let result = provider.build(&config).await;
        assert!(
            result.is_ok(),
            "Build with no_cache should succeed: {:?}",
            result
        );

        // Cleanup
        let _ = provider
            .run_cmd(&["rmi", "-f", "devc-test-nocache:latest"])
            .await;
    }
}
