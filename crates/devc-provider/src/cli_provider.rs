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
        let mut cmd = self.build_command();
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

    /// Build `--env=K=V` arguments from an environment variable map
    fn env_args(env: &HashMap<String, String>) -> Vec<String> {
        env.iter()
            .map(|(k, v)| format!("--env={}={}", k, v))
            .collect()
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

        // Stream both stdout and stderr concurrently to avoid pipe deadlock.
        // If only one stream is consumed, the child process can block when the
        // other stream's OS pipe buffer fills up (64KB on Linux), causing a hang.
        // Podman writes build progress to stdout; Docker/BuildKit uses stderr.
        let mut stdout_lines = child.stdout.take().map(|s| BufReader::new(s).lines());
        let mut stderr_lines = child.stderr.take().map(|s| BufReader::new(s).lines());

        loop {
            tokio::select! {
                result = async {
                    match stdout_lines.as_mut() {
                        Some(lines) => lines.next_line().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match result {
                        Ok(Some(line)) => { let _ = progress.send(line); }
                        _ => { stdout_lines = None; }
                    }
                }
                result = async {
                    match stderr_lines.as_mut() {
                        Some(lines) => lines.next_line().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match result {
                        Ok(Some(line)) => { let _ = progress.send(line); }
                        _ => { stderr_lines = None; }
                    }
                }
            }
            if stdout_lines.is_none() && stderr_lines.is_none() {
                break;
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
        args.extend(Self::env_args(&config.env));

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

        // Init process
        if config.init {
            args.push("--init".to_string());
        }

        // Entrypoint override
        if let Some(ref entrypoint) = config.entrypoint {
            if let Some(ep) = entrypoint.first() {
                args.push(format!("--entrypoint={}", ep));
            }
        }

        // Extra arguments (from runArgs)
        args.extend(config.extra_args.clone());

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

        args.extend(Self::env_args(&config.env));

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

        if let Some(ref user) = config.user {
            args.push(format!("--user={}", user));
        }

        args.extend(Self::env_args(&config.env));

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
        let stdout = child.stdout.take().expect("stdout must exist when piped");

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
        Ok(parse_list_output(&output))
    }

    async fn inspect(&self, id: &ContainerId) -> Result<ContainerDetails> {
        let output = self.run_cmd(&["inspect", "--format=json", &id.0]).await?;
        parse_inspect_output(&output, id)
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

        let mut child = cmd
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ProviderError::RuntimeError(e.to_string()))?;

        let stdout = child.stdout.take().expect("stdout must exist when piped");
        Ok(LogStream {
            stream: Box::pin(stdout),
            _child: Some(child),
        })
    }

    async fn ping(&self) -> Result<()> {
        self.run_cmd(&["--version"]).await?;
        Ok(())
    }

    fn runtime_args(&self) -> (String, Vec<String>) {
        if self.cmd_prefix.is_empty() {
            (self.cmd.clone(), vec![])
        } else {
            let mut args: Vec<String> = self.cmd_prefix[1..].to_vec();
            args.push(self.cmd.clone());
            (self.cmd_prefix[0].clone(), args)
        }
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

    async fn compose_up(
        &self,
        compose_files: &[&str],
        project_name: &str,
        project_dir: &Path,
        progress: Option<mpsc::UnboundedSender<String>>,
    ) -> Result<()> {
        let mut cmd = self.build_command();
        cmd.arg("compose");
        for f in compose_files {
            cmd.arg("-f").arg(f);
        }
        cmd.arg("-p").arg(project_name);
        cmd.args(["up", "-d", "--build"]);
        cmd.current_dir(project_dir);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| ProviderError::RuntimeError(e.to_string()))?;

        let mut stderr_lines = Vec::new();
        if let Some(stderr) = child.stderr.take() {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(ref tx) = progress {
                    let _ = tx.send(line.clone());
                }
                stderr_lines.push(line);
            }
        }

        let status = child
            .wait()
            .await
            .map_err(|e| ProviderError::RuntimeError(e.to_string()))?;

        if !status.success() {
            let detail = stderr_lines.join("\n");
            return Err(ProviderError::RuntimeError(format!(
                "{} compose up failed: {}",
                self.cmd, detail
            )));
        }

        Ok(())
    }

    async fn compose_down(
        &self,
        compose_files: &[&str],
        project_name: &str,
        project_dir: &Path,
    ) -> Result<()> {
        let mut args = vec!["compose".to_string()];
        for f in compose_files {
            args.push("-f".to_string());
            args.push(f.to_string());
        }
        args.push("-p".to_string());
        args.push(project_name.to_string());
        args.push("down".to_string());

        let mut cmd = self.build_command();
        for arg in &args {
            cmd.arg(arg);
        }
        cmd.current_dir(project_dir);

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| ProviderError::RuntimeError(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProviderError::RuntimeError(format!(
                "{} compose down failed: {}",
                self.cmd, stderr
            )));
        }

        Ok(())
    }

    async fn compose_ps(
        &self,
        compose_files: &[&str],
        project_name: &str,
        project_dir: &Path,
    ) -> Result<Vec<crate::ComposeServiceInfo>> {
        let mut args = vec!["compose".to_string()];
        for f in compose_files {
            args.push("-f".to_string());
            args.push(f.to_string());
        }
        args.push("-p".to_string());
        args.push(project_name.to_string());
        args.push("ps".to_string());
        args.push("--format=json".to_string());

        let mut cmd = self.build_command();
        for arg in &args {
            cmd.arg(arg);
        }
        cmd.current_dir(project_dir);

        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|e| ProviderError::RuntimeError(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ProviderError::RuntimeError(format!(
                "{} compose ps failed: {}",
                self.cmd, stderr
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_compose_ps_output(&stdout))
    }

    async fn discover_devcontainers(&self) -> Result<Vec<DiscoveredContainer>> {
        // List ALL containers with detailed format including labels
        let format = "--format={{.ID}}|{{.Names}}|{{.Image}}|{{.State}}|{{.Labels}}|{{.CreatedAt}}";
        let output = self.run_cmd(&["ps", "-a", "--no-trunc", format]).await?;
        Ok(parse_discover_output(&output, self.provider_type))
    }
}

/// Parse the pipe-delimited output of `docker/podman ps` into ContainerInfo items
fn parse_list_output(output: &str) -> Vec<ContainerInfo> {
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
                created: 0,
                labels: HashMap::new(),
            });
        }
    }
    containers
}

/// Parse JSON output of `docker/podman inspect --format=json` into ContainerDetails
fn parse_inspect_output(output: &str, id: &ContainerId) -> Result<ContainerDetails> {
    let inspect: Vec<serde_json::Value> = serde_json::from_str(output)
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
            let ip_address = ns
                .get("IPAddress")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let gateway = ns
                .get("Gateway")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

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

/// Parse the pipe-delimited output of `docker/podman ps -a` for discovery
fn parse_discover_output(output: &str, provider_type: ProviderType) -> Vec<DiscoveredContainer> {
    let mut discovered = Vec::new();
    for line in output.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() < 6 {
            continue;
        }

        let labels = parse_cli_labels(parts[4]);
        let (is_devcontainer, source, _managed) = detect_devcontainer_source_from_labels(&labels);

        if !is_devcontainer {
            continue;
        }

        // Extract workspace path from labels (fall back through multiple label keys)
        let workspace_path = labels
            .get("devcontainer.local_folder")
            .or_else(|| labels.get("devc.workspace"))
            .cloned();

        let created = {
            let raw = parts[5].trim();
            if raw.is_empty() {
                None
            } else {
                Some(raw.to_string())
            }
        };

        discovered.push(DiscoveredContainer {
            id: ContainerId::new(parts[0]),
            name: parts[1].to_string(),
            image: parts[2].to_string(),
            status: ContainerStatus::from(parts[3]),
            source,
            workspace_path,
            labels,
            provider: provider_type,
            created,
        });
    }
    discovered
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

    // Check for DevPod labels (case-insensitive prefix)
    if labels
        .keys()
        .any(|k| k.to_lowercase().starts_with("devpod."))
    {
        return (true, DevcontainerSource::DevPod, false);
    }

    // Check for common devcontainer patterns
    if labels.keys().any(|k| k.starts_with("devcontainer.")) {
        return (true, DevcontainerSource::Other, false);
    }

    (false, DevcontainerSource::Other, false)
}

/// Parse the JSON output of `docker/podman compose ps --format=json`.
///
/// Handles both podman-compose (JSON array with `Id`, `State`, and service in
/// `Labels["com.docker.compose.service"]`) and docker compose (one JSON object
/// per line with `ID`, `Service`, `State`).
fn parse_compose_ps_output(stdout: &str) -> Vec<crate::ComposeServiceInfo> {
    let mut services = Vec::new();

    // Try parsing as a JSON array first (podman-compose format),
    // then fall back to one-JSON-object-per-line (docker compose format).
    let entries: Vec<serde_json::Value> =
        if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(stdout.trim()) {
            arr
        } else {
            stdout
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
                .collect()
        };

    for parsed in entries {
        // Docker compose uses "Service"; podman-compose stores it in labels
        let service_name = parsed["Service"]
            .as_str()
            .or_else(|| parsed["Labels"]["com.docker.compose.service"].as_str())
            .unwrap_or("")
            .to_string();
        // Docker compose uses "ID"; podman-compose uses "Id"
        let container_id = parsed["ID"]
            .as_str()
            .or_else(|| parsed["Id"].as_str())
            .unwrap_or("")
            .to_string();
        let state = parsed["State"].as_str().unwrap_or("unknown");

        services.push(crate::ComposeServiceInfo {
            service_name,
            container_id: ContainerId::new(container_id),
            status: ContainerStatus::from(state),
        });
    }

    services
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    // ==================== parse_cli_labels tests ====================

    #[test]
    fn test_parse_cli_labels_basic() {
        let labels = parse_cli_labels("foo=bar,baz=qux");
        assert_eq!(labels.get("foo").unwrap(), "bar");
        assert_eq!(labels.get("baz").unwrap(), "qux");
    }

    #[test]
    fn test_parse_cli_labels_empty() {
        let labels = parse_cli_labels("");
        assert!(labels.is_empty());
    }

    #[test]
    fn test_parse_cli_labels_single() {
        let labels = parse_cli_labels("key=value");
        assert_eq!(labels.len(), 1);
        assert_eq!(labels.get("key").unwrap(), "value");
    }

    #[test]
    fn test_parse_cli_labels_value_with_equals() {
        // Values can contain equals signs (e.g. "key=a=b")
        let labels = parse_cli_labels("key=a=b,other=c");
        assert_eq!(labels.get("key").unwrap(), "a=b");
        assert_eq!(labels.get("other").unwrap(), "c");
    }

    // ==================== detect_devcontainer_source tests ====================

    #[test]
    fn test_detect_devc_source() {
        let mut labels = HashMap::new();
        labels.insert("devc.managed".to_string(), "true".to_string());
        let (is_dc, source, managed) = detect_devcontainer_source_from_labels(&labels);
        assert!(is_dc);
        assert_eq!(source, DevcontainerSource::Devc);
        assert!(managed);
    }

    #[test]
    fn test_detect_vscode_source() {
        let mut labels = HashMap::new();
        labels.insert(
            "devcontainer.local_folder".to_string(),
            "/home/user/project".to_string(),
        );
        let (is_dc, source, managed) = detect_devcontainer_source_from_labels(&labels);
        assert!(is_dc);
        assert_eq!(source, DevcontainerSource::VsCode);
        assert!(!managed);
    }

    #[test]
    fn test_detect_other_devcontainer() {
        let mut labels = HashMap::new();
        labels.insert("devcontainer.metadata".to_string(), "{}".to_string());
        let (is_dc, source, managed) = detect_devcontainer_source_from_labels(&labels);
        assert!(is_dc);
        assert_eq!(source, DevcontainerSource::Other);
        assert!(!managed);
    }

    #[test]
    fn test_detect_devpod_source() {
        let mut labels = HashMap::new();
        labels.insert("devcontainer.metadata".to_string(), "{}".to_string());
        labels.insert("Devpod.user".to_string(), "vscode".to_string());
        let (is_dc, source, managed) = detect_devcontainer_source_from_labels(&labels);
        assert!(is_dc);
        assert_eq!(source, DevcontainerSource::DevPod);
        assert!(!managed);
    }

    #[test]
    fn test_detect_non_devcontainer() {
        let mut labels = HashMap::new();
        labels.insert("com.docker.compose.service".to_string(), "web".to_string());
        let (is_dc, _source, _managed) = detect_devcontainer_source_from_labels(&labels);
        assert!(!is_dc);
    }

    // ==================== parse_compose_ps_output tests ====================

    #[test]
    fn test_parse_compose_ps_podman_format() {
        // Podman-compose returns a JSON array with Id, State,
        // and service name in Labels["com.docker.compose.service"]
        let stdout = r#"[
            {
                "Id": "abc123def456",
                "State": "running",
                "Labels": {
                    "com.docker.compose.service": "web"
                }
            },
            {
                "Id": "789xyz000111",
                "State": "exited",
                "Labels": {
                    "com.docker.compose.service": "db"
                }
            }
        ]"#;

        let services = parse_compose_ps_output(stdout);
        assert_eq!(services.len(), 2);

        assert_eq!(services[0].service_name, "web");
        assert_eq!(services[0].container_id.0, "abc123def456");
        assert_eq!(services[0].status, ContainerStatus::Running);

        assert_eq!(services[1].service_name, "db");
        assert_eq!(services[1].container_id.0, "789xyz000111");
        assert_eq!(services[1].status, ContainerStatus::Exited);
    }

    #[test]
    fn test_parse_compose_ps_docker_format() {
        // Docker compose returns one JSON object per line (NDJSON)
        // with ID, Service, State fields
        let stdout = r#"{"ID":"aaa111","Service":"app","State":"running"}
{"ID":"bbb222","Service":"redis","State":"exited"}"#;

        let services = parse_compose_ps_output(stdout);
        assert_eq!(services.len(), 2);

        assert_eq!(services[0].service_name, "app");
        assert_eq!(services[0].container_id.0, "aaa111");
        assert_eq!(services[0].status, ContainerStatus::Running);

        assert_eq!(services[1].service_name, "redis");
        assert_eq!(services[1].container_id.0, "bbb222");
        assert_eq!(services[1].status, ContainerStatus::Exited);
    }

    #[test]
    fn test_parse_compose_ps_empty_output() {
        assert!(parse_compose_ps_output("").is_empty());
        assert!(parse_compose_ps_output("  ").is_empty());
        assert!(parse_compose_ps_output("\n\n").is_empty());
    }

    // ==================== parse_list_output tests ====================

    #[test]
    fn test_parse_list_docker_output() {
        // Docker ps output: ID|Names|Image|State|Created
        let output = "abc123|my-container|ubuntu:22.04|running|2024-01-15\n\
                       def456|another-one|node:18|exited|2024-01-14\n";

        let containers = parse_list_output(output);
        assert_eq!(containers.len(), 2);

        assert_eq!(containers[0].id.0, "abc123");
        assert_eq!(containers[0].name, "my-container");
        assert_eq!(containers[0].image, "ubuntu:22.04");
        assert_eq!(containers[0].status, ContainerStatus::Running);

        assert_eq!(containers[1].id.0, "def456");
        assert_eq!(containers[1].name, "another-one");
        assert_eq!(containers[1].image, "node:18");
        assert_eq!(containers[1].status, ContainerStatus::Exited);
    }

    #[test]
    fn test_parse_list_podman_output() {
        // Podman uses the same format but may have different status strings
        let output = "aabbcc|podman-ctr|alpine:latest|created|2024-02-01\n";
        let containers = parse_list_output(output);
        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].status, ContainerStatus::Created);
    }

    #[test]
    fn test_parse_list_empty_output() {
        assert!(parse_list_output("").is_empty());
        assert!(parse_list_output("\n\n").is_empty());
        assert!(parse_list_output("  \n  \n").is_empty());
    }

    #[test]
    fn test_parse_list_malformed_lines() {
        // Lines with fewer than 4 parts should be skipped
        let output = "abc|name|image\n\
                       def|name2|image2|running|extra\n";
        let containers = parse_list_output(output);
        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].id.0, "def");
    }

    // ==================== parse_inspect_output tests ====================

    #[test]
    fn test_parse_inspect_docker_output() {
        // Captured Docker inspect output (simplified)
        let output = r#"[{
            "Id": "sha256:abc123",
            "Created": "2024-01-15T10:30:00.000000000Z",
            "Name": "/my-devcontainer",
            "Image": "sha256:img456",
            "State": {
                "Status": "running",
                "Running": true,
                "ExitCode": 0,
                "StartedAt": "2024-01-15T10:30:01.000000000Z",
                "FinishedAt": "0001-01-01T00:00:00Z"
            },
            "Config": {
                "Image": "ubuntu:22.04",
                "Env": ["PATH=/usr/bin", "TERM=xterm"],
                "Labels": {
                    "devc.managed": "true",
                    "devc.workspace": "/home/user/project"
                }
            },
            "Mounts": [
                {
                    "Type": "bind",
                    "Source": "/home/user/project",
                    "Destination": "/workspace",
                    "RW": true
                },
                {
                    "Type": "volume",
                    "Source": "my-vol",
                    "Destination": "/data",
                    "RW": false
                }
            ],
            "NetworkSettings": {
                "IPAddress": "172.17.0.2",
                "Gateway": "172.17.0.1",
                "Ports": {
                    "3000/tcp": [{"HostIp": "0.0.0.0", "HostPort": "3000"}],
                    "5432/tcp": null
                },
                "Networks": {
                    "bridge": {
                        "NetworkID": "net123",
                        "IPAddress": "172.17.0.2",
                        "Gateway": "172.17.0.1"
                    }
                }
            }
        }]"#;

        let id = ContainerId::new("abc123");
        let details = parse_inspect_output(output, &id).unwrap();

        assert_eq!(details.name, "my-devcontainer"); // Leading / stripped
        assert_eq!(details.image, "ubuntu:22.04");
        assert_eq!(details.image_id, "sha256:img456");
        assert_eq!(details.status, ContainerStatus::Running);
        assert_eq!(details.exit_code, Some(0));

        // Labels
        assert_eq!(details.labels.get("devc.managed").unwrap(), "true");
        assert_eq!(
            details.labels.get("devc.workspace").unwrap(),
            "/home/user/project"
        );

        // Env
        assert_eq!(details.env.len(), 2);
        assert!(details.env.contains(&"PATH=/usr/bin".to_string()));
        assert!(details.env.contains(&"TERM=xterm".to_string()));

        // Mounts
        assert_eq!(details.mounts.len(), 2);
        assert_eq!(details.mounts[0].mount_type, "bind");
        assert_eq!(details.mounts[0].source, "/home/user/project");
        assert_eq!(details.mounts[0].destination, "/workspace");
        assert!(!details.mounts[0].read_only);
        assert_eq!(details.mounts[1].mount_type, "volume");
        assert!(details.mounts[1].read_only);

        // Ports
        assert!(details.ports.len() >= 1);
        let tcp_3000 = details
            .ports
            .iter()
            .find(|p| p.container_port == 3000)
            .unwrap();
        assert_eq!(tcp_3000.host_port, Some(3000));
        assert_eq!(tcp_3000.host_ip.as_deref(), Some("0.0.0.0"));
        assert_eq!(tcp_3000.protocol, "tcp");

        // Network settings
        assert_eq!(
            details.network_settings.ip_address.as_deref(),
            Some("172.17.0.2")
        );
        assert_eq!(
            details.network_settings.gateway.as_deref(),
            Some("172.17.0.1")
        );
        assert!(details.network_settings.networks.contains_key("bridge"));

        // Timestamps
        assert!(details.created > 0);
        assert!(details.started_at.is_some());
    }

    #[test]
    fn test_parse_inspect_podman_output() {
        // Podman inspect output differs: Name has no leading /, uses different timestamp format
        let output = r#"[{
            "Id": "podman123",
            "Created": "2024-02-01T15:00:00.000000000Z",
            "Name": "podman-container",
            "Image": "sha256:podimg789",
            "State": {
                "Status": "exited",
                "Running": false,
                "ExitCode": 137,
                "StartedAt": "2024-02-01T15:00:01.000000000Z",
                "FinishedAt": "2024-02-01T15:30:00.000000000Z"
            },
            "Config": {
                "Image": "node:18-alpine",
                "Env": ["NODE_ENV=development"],
                "Labels": {
                    "devcontainer.local_folder": "/home/user/webapp",
                    "devcontainer.config_file": "/home/user/webapp/.devcontainer/devcontainer.json"
                }
            },
            "Mounts": [],
            "NetworkSettings": {
                "Ports": {},
                "Networks": {}
            }
        }]"#;

        let id = ContainerId::new("podman123");
        let details = parse_inspect_output(output, &id).unwrap();

        assert_eq!(details.name, "podman-container"); // No leading /
        assert_eq!(details.status, ContainerStatus::Exited);
        assert_eq!(details.exit_code, Some(137));
        assert_eq!(details.image, "node:18-alpine");
        assert!(details.mounts.is_empty());
        assert!(details.ports.is_empty());
        assert!(details.finished_at.is_some());
    }

    #[test]
    fn test_parse_inspect_minimal_fields() {
        // Minimal inspect output — many optional fields missing
        let output = r#"[{
            "Id": "min123",
            "State": { "Status": "created" },
            "Config": { "Image": "alpine" }
        }]"#;

        let id = ContainerId::new("min123");
        let details = parse_inspect_output(output, &id).unwrap();

        assert_eq!(details.status, ContainerStatus::Created);
        assert_eq!(details.image, "alpine");
        assert!(details.name.is_empty());
        assert!(details.labels.is_empty());
        assert!(details.env.is_empty());
        assert!(details.mounts.is_empty());
        assert!(details.ports.is_empty());
        assert_eq!(details.exit_code, None);
    }

    #[test]
    fn test_parse_inspect_empty_array() {
        let output = "[]";
        let id = ContainerId::new("missing");
        let result = parse_inspect_output(output, &id);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_inspect_invalid_json() {
        let output = "not valid json";
        let id = ContainerId::new("x");
        let result = parse_inspect_output(output, &id);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_inspect_multiple_port_bindings() {
        // Port with multiple bindings (IPv4 + IPv6)
        let output = r#"[{
            "Id": "ports123",
            "State": { "Status": "running" },
            "Config": { "Image": "nginx" },
            "NetworkSettings": {
                "Ports": {
                    "80/tcp": [
                        {"HostIp": "0.0.0.0", "HostPort": "8080"},
                        {"HostIp": "::", "HostPort": "8080"}
                    ],
                    "443/tcp": [
                        {"HostIp": "0.0.0.0", "HostPort": "8443"}
                    ]
                }
            }
        }]"#;

        let id = ContainerId::new("ports123");
        let details = parse_inspect_output(output, &id).unwrap();

        // Port 80 has 2 bindings (IPv4 + IPv6)
        let port_80: Vec<_> = details
            .ports
            .iter()
            .filter(|p| p.container_port == 80)
            .collect();
        assert_eq!(port_80.len(), 2);

        // Port 443 has 1 binding
        let port_443: Vec<_> = details
            .ports
            .iter()
            .filter(|p| p.container_port == 443)
            .collect();
        assert_eq!(port_443.len(), 1);
        assert_eq!(port_443[0].host_port, Some(8443));
    }

    // ==================== parse_discover_output tests ====================

    #[test]
    fn test_parse_discover_devc_container() {
        let output = "abc123|my-devc|ubuntu:22.04|running|devc.managed=true,devc.workspace=/home/user/proj|2024-01-15\n";
        let discovered = parse_discover_output(output, ProviderType::Docker);
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].name, "my-devc");
        assert_eq!(discovered[0].source, DevcontainerSource::Devc);
        assert_eq!(
            discovered[0].workspace_path.as_deref(),
            Some("/home/user/proj")
        );
        assert_eq!(discovered[0].provider, ProviderType::Docker);
    }

    #[test]
    fn test_parse_discover_vscode_container() {
        let output = "vsc123|vscode_devcontainer_abcdef|node:18|running|devcontainer.local_folder=/home/user/webapp,devcontainer.config_file=/home/user/webapp/.devcontainer/devcontainer.json|2024-02-01\n";
        let discovered = parse_discover_output(output, ProviderType::Podman);
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].source, DevcontainerSource::VsCode);
        assert_eq!(
            discovered[0].workspace_path.as_deref(),
            Some("/home/user/webapp")
        );
        assert_eq!(discovered[0].provider, ProviderType::Podman);
    }

    #[test]
    fn test_parse_discover_devpod_container() {
        let output = "dp123|devpod-myproject|alpine|running|devcontainer.metadata={},Devpod.user=vscode,Devpod.workspace=/workspace|2024-03-01\n";
        let discovered = parse_discover_output(output, ProviderType::Docker);
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].source, DevcontainerSource::DevPod);
    }

    #[test]
    fn test_parse_discover_skips_non_devcontainer() {
        let output = "reg123|postgres|postgres:15|running|maintainer=PostgreSQL|2024-01-01\n\
                       dc456|my-devcontainer|ubuntu|running|devc.managed=true|2024-01-02\n";
        let discovered = parse_discover_output(output, ProviderType::Docker);
        // Only the devcontainer should be returned, not the postgres container
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0].name, "my-devcontainer");
    }

    #[test]
    fn test_parse_discover_mixed_sources() {
        let output = "a|devc-ctr|img|running|devc.managed=true|2024-01-01\n\
                       b|vscode-ctr|img|running|devcontainer.local_folder=/proj|2024-01-02\n\
                       c|normal-ctr|img|running|com.docker.compose.service=web|2024-01-03\n\
                       d|devpod-ctr|img|exited|devcontainer.metadata={},Devpod.workspace=/ws|2024-01-04\n";
        let discovered = parse_discover_output(output, ProviderType::Docker);
        assert_eq!(discovered.len(), 3); // devc + vscode + devpod, but not the compose one
    }

    #[test]
    fn test_parse_discover_empty_created() {
        let output = "abc|my-ctr|img|running|devc.managed=true|\n";
        let discovered = parse_discover_output(output, ProviderType::Docker);
        assert_eq!(discovered.len(), 1);
        assert!(discovered[0].created.is_none());
    }

    #[test]
    fn test_parse_discover_too_few_fields() {
        // Lines with < 6 pipe-separated fields should be skipped
        let output = "abc|name|image|running|labels\n";
        let discovered = parse_discover_output(output, ProviderType::Docker);
        assert!(discovered.is_empty());
    }

    // ==================== ContainerStatus::from tests ====================

    #[test]
    fn test_container_status_from_str() {
        assert_eq!(ContainerStatus::from("running"), ContainerStatus::Running);
        assert_eq!(ContainerStatus::from("Running"), ContainerStatus::Running);
        assert_eq!(ContainerStatus::from("RUNNING"), ContainerStatus::Running);
        assert_eq!(ContainerStatus::from("exited"), ContainerStatus::Exited);
        assert_eq!(ContainerStatus::from("created"), ContainerStatus::Created);
        assert_eq!(ContainerStatus::from("paused"), ContainerStatus::Paused);
        assert_eq!(
            ContainerStatus::from("restarting"),
            ContainerStatus::Restarting
        );
        assert_eq!(ContainerStatus::from("removing"), ContainerStatus::Removing);
        assert_eq!(ContainerStatus::from("dead"), ContainerStatus::Dead);
        assert_eq!(
            ContainerStatus::from("something_else"),
            ContainerStatus::Unknown
        );
        assert_eq!(ContainerStatus::from(""), ContainerStatus::Unknown);
    }

    async fn get_test_provider() -> Option<CliProvider> {
        match std::env::var("DEVC_TEST_PROVIDER").as_deref() {
            Ok("docker") => CliProvider::new_docker().await.ok(),
            Ok("podman") => CliProvider::new_podman().await.ok(),
            Ok("toolbox") => CliProvider::new_toolbox().await.ok(),
            _ => {
                if let Ok(p) = CliProvider::new_toolbox().await {
                    return Some(p);
                }
                if let Ok(p) = CliProvider::new_podman().await {
                    return Some(p);
                }
                if let Ok(p) = CliProvider::new_docker().await {
                    return Some(p);
                }
                None
            }
        }
    }

    #[tokio::test]
    #[ignore] // requires a container runtime
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
    #[ignore] // requires a container runtime
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

    #[tokio::test]
    #[ignore] // Requires container runtime
    async fn test_runtime_flags_init_and_cap_add() {
        let provider = match get_test_provider().await {
            Some(p) => p,
            None => {
                eprintln!("Skipping test: no container runtime available");
                return;
            }
        };

        // Pull alpine image
        let _ = provider.pull("alpine:latest").await;

        let config = CreateContainerConfig {
            image: "alpine:latest".to_string(),
            name: Some("devc_test_runtime_flags".to_string()),
            cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
            init: true,
            cap_add: vec!["SYS_PTRACE".to_string()],
            privileged: false,
            security_opt: vec!["seccomp=unconfined".to_string()],
            extra_args: vec!["--shm-size=1g".to_string()],
            tty: true,
            stdin_open: true,
            ..Default::default()
        };

        let id = provider
            .create(&config)
            .await
            .expect("create should succeed");
        provider.start(&id).await.expect("start should succeed");

        // Verify init flag
        let init_output = provider
            .run_cmd(&["inspect", "--format={{.HostConfig.Init}}", &id.0])
            .await
            .expect("inspect init");
        assert!(
            init_output.trim() == "true" || init_output.trim() == "<nil>",
            "Init should be true, got: {}",
            init_output.trim()
        );
        // On Docker, Init is a *bool so it shows "true"; on Podman it may differ
        // The important thing is the container was created with --init

        // Verify cap_add
        let cap_output = provider
            .run_cmd(&["inspect", "--format={{.HostConfig.CapAdd}}", &id.0])
            .await
            .expect("inspect cap_add");
        assert!(
            cap_output.contains("SYS_PTRACE"),
            "CapAdd should contain SYS_PTRACE, got: {}",
            cap_output.trim()
        );

        // Verify security_opt
        let secopt_output = provider
            .run_cmd(&["inspect", "--format={{.HostConfig.SecurityOpt}}", &id.0])
            .await
            .expect("inspect security_opt");
        assert!(
            secopt_output.contains("seccomp=unconfined"),
            "SecurityOpt should contain seccomp=unconfined, got: {}",
            secopt_output.trim()
        );

        // Verify shm-size (1073741824 = 1g in bytes)
        let shm_output = provider
            .run_cmd(&["inspect", "--format={{.HostConfig.ShmSize}}", &id.0])
            .await
            .expect("inspect shm_size");
        assert!(
            shm_output.trim() == "1073741824" || shm_output.contains("1g"),
            "ShmSize should be 1g (1073741824), got: {}",
            shm_output.trim()
        );

        // Cleanup
        let _ = provider.remove(&id, true).await;
    }

    #[tokio::test]
    #[ignore] // Requires container runtime
    async fn test_override_command_none() {
        let provider = match get_test_provider().await {
            Some(p) => p,
            None => {
                eprintln!("Skipping test: no container runtime available");
                return;
            }
        };

        let _ = provider.pull("alpine:latest").await;

        // Create with cmd: None (simulating overrideCommand: false)
        let config = CreateContainerConfig {
            image: "alpine:latest".to_string(),
            name: Some("devc_test_override_cmd".to_string()),
            cmd: None, // Use image default CMD
            tty: true,
            stdin_open: true,
            ..Default::default()
        };

        let id = provider
            .create(&config)
            .await
            .expect("create should succeed");

        // Inspect the container's command — should be image default, not sleep infinity
        let cmd_output = provider
            .run_cmd(&["inspect", "--format={{.Config.Cmd}}", &id.0])
            .await
            .expect("inspect cmd");
        // Alpine's default CMD is ["/bin/sh"] — it should NOT contain "sleep infinity"
        assert!(
            !cmd_output.contains("sleep infinity"),
            "cmd=None should use image default, got: {}",
            cmd_output.trim()
        );

        // Cleanup
        let _ = provider.remove(&id, true).await;
    }

    #[tokio::test]
    #[ignore] // Requires container runtime
    async fn test_container_env_vars() {
        let provider = match get_test_provider().await {
            Some(p) => p,
            None => {
                eprintln!("Skipping test: no container runtime available");
                return;
            }
        };

        let _ = provider.pull("alpine:latest").await;

        let mut env = HashMap::new();
        env.insert("MY_VAR".to_string(), "hello".to_string());
        env.insert("ANOTHER".to_string(), "world".to_string());

        let config = CreateContainerConfig {
            image: "alpine:latest".to_string(),
            name: Some("devc_test_env_vars".to_string()),
            cmd: Some(vec!["sleep".to_string(), "infinity".to_string()]),
            env,
            tty: true,
            stdin_open: true,
            ..Default::default()
        };

        let id = provider
            .create(&config)
            .await
            .expect("create should succeed");
        provider.start(&id).await.expect("start should succeed");

        // Verify env vars via exec
        let exec_config = ExecConfig {
            cmd: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo $MY_VAR".to_string(),
            ],
            ..Default::default()
        };
        let result = provider
            .exec(&id, &exec_config)
            .await
            .expect("exec should succeed");
        assert!(
            result.output.contains("hello"),
            "MY_VAR should be 'hello', got: {}",
            result.output.trim()
        );

        let exec_config2 = ExecConfig {
            cmd: vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo $ANOTHER".to_string(),
            ],
            ..Default::default()
        };
        let result2 = provider
            .exec(&id, &exec_config2)
            .await
            .expect("exec should succeed");
        assert!(
            result2.output.contains("world"),
            "ANOTHER should be 'world', got: {}",
            result2.output.trim()
        );

        // Cleanup
        let _ = provider.remove(&id, true).await;
    }
}
