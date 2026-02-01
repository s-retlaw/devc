//! Host Podman provider for Fedora Toolbox environments
//!
//! Uses `flatpak-spawn --host podman` to run commands on the host system.

use crate::{
    BuildConfig, ContainerDetails, ContainerId, ContainerInfo, ContainerProvider, ContainerStatus,
    CreateContainerConfig, DevcontainerSource, DiscoveredContainer, ExecConfig, ExecResult,
    ExecStream, ImageId, LogConfig, LogStream, MountType, NetworkSettings, ProviderError,
    ProviderInfo, ProviderType, Result,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::pin::Pin;
use std::process::Stdio;
use tokio::process::Command;

/// Host Podman provider using flatpak-spawn
pub struct HostPodmanProvider {
    /// Command prefix (e.g., ["flatpak-spawn", "--host", "podman"])
    cmd_prefix: Vec<String>,
}

impl HostPodmanProvider {
    /// Create a new host podman provider
    pub async fn new() -> Result<Self> {
        let provider = Self {
            cmd_prefix: vec![
                "flatpak-spawn".to_string(),
                "--host".to_string(),
                "podman".to_string(),
            ],
        };

        // Test connection
        provider.ping().await?;

        Ok(provider)
    }

    /// Run a podman command and get output
    async fn run_cmd(&self, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new(&self.cmd_prefix[0]);
        for prefix_arg in &self.cmd_prefix[1..] {
            cmd.arg(prefix_arg);
        }
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
}

#[async_trait]
impl ContainerProvider for HostPodmanProvider {
    async fn build(&self, config: &BuildConfig) -> Result<ImageId> {
        let context = config.context.to_string_lossy();
        let dockerfile = format!("-f={}", config.dockerfile);
        let tag = format!("-t={}", config.tag);

        let mut args = vec!["build", &dockerfile, &tag];

        if config.no_cache {
            args.push("--no-cache");
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

        args.push(&context);

        let output = self.run_cmd(&args).await?;
        tracing::debug!("Build output: {}", output);

        // Get the image ID
        let inspect_output = self.run_cmd(&["inspect", "--format={{.Id}}", &config.tag]).await?;
        Ok(ImageId::new(inspect_output.trim()))
    }

    async fn pull(&self, image: &str) -> Result<ImageId> {
        self.run_cmd(&["pull", image]).await?;

        let output = self.run_cmd(&["inspect", "--format={{.Id}}", image]).await?;
        Ok(ImageId::new(output.trim()))
    }

    async fn create(&self, config: &CreateContainerConfig) -> Result<ContainerId> {
        let mut args = vec!["create".to_string()];

        // Use keep-id to map host user into container for proper file permissions
        // This is essential for rootless podman with bind mounts
        args.push("--userns=keep-id".to_string());

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
        // Use :Z for SELinux relabeling on bind mounts (required on Fedora/RHEL)
        for mount in &config.mounts {
            let mount_str = match mount.mount_type {
                MountType::Bind => {
                    // Use -v syntax with :Z for SELinux relabeling
                    let ro = if mount.read_only { ":ro" } else { "" };
                    format!("-v={}:{}:Z{}", mount.source, mount.target, ro)
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

        let mut cmd = Command::new(&self.cmd_prefix[0]);
        for prefix_arg in &self.cmd_prefix[1..] {
            cmd.arg(prefix_arg);
        }
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

    async fn exec_interactive(
        &self,
        id: &ContainerId,
        config: &ExecConfig,
    ) -> Result<ExecStream> {
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

        let mut cmd = Command::new(&self.cmd_prefix[0]);
        for prefix_arg in &self.cmd_prefix[1..] {
            cmd.arg(prefix_arg);
        }
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
                    .filter_map(|(k, v)| {
                        v.as_str().map(|s| (k.clone(), s.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(ContainerDetails {
            id: id.clone(),
            name,
            image,
            image_id,
            status,
            created: 0,
            started_at: None,
            finished_at: None,
            exit_code,
            labels,
            env: Vec::new(),
            mounts: Vec::new(),
            ports: Vec::new(),
            network_settings: NetworkSettings::default(),
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

        let mut cmd = Command::new(&self.cmd_prefix[0]);
        for prefix_arg in &self.cmd_prefix[1..] {
            cmd.arg(prefix_arg);
        }
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
            provider_type: ProviderType::Podman,
            version: "host".to_string(),
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

            let labels = parse_podman_labels(parts[4]);
            let (is_devcontainer, source, managed) = detect_devcontainer_source_from_labels(&labels);

            if !is_devcontainer {
                continue;
            }

            // Extract workspace path from labels
            let workspace_path = labels
                .get("devcontainer.local_folder")
                .cloned();

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

/// Parse podman labels format "key=value,key2=value2" into HashMap
fn parse_podman_labels(label_str: &str) -> HashMap<String, String> {
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
        || labels.get("vscode.devcontainer").map(|v| v == "true").unwrap_or(false)
    {
        return (true, DevcontainerSource::VsCode, false);
    }

    // Check for common devcontainer patterns
    if labels.keys().any(|k| k.starts_with("devcontainer.")) {
        return (true, DevcontainerSource::Other, false);
    }

    (false, DevcontainerSource::Other, false)
}
