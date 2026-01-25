//! Docker provider implementation using bollard

use crate::{
    BuildConfig, ContainerDetails, ContainerId, ContainerInfo, ContainerProvider, ContainerStatus,
    CreateContainerConfig, ExecConfig, ExecResult, ExecStream, ImageId, LogConfig, LogStream,
    MountInfo, MountType, NetworkInfo, NetworkSettings, PortInfo, ProviderError, ProviderInfo,
    ProviderType, Result,
};
use async_trait::async_trait;
use bollard::container::{
    Config, CreateContainerOptions, ListContainersOptions, LogsOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::image::BuildImageOptions;
use bollard::service::{HostConfig, Mount, PortBinding};
use bollard::Docker;
use futures::StreamExt;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::pin::Pin;
use tokio::io::AsyncRead;

/// Docker provider using bollard crate
pub struct DockerProvider {
    client: Docker,
    provider_type: ProviderType,
}

impl DockerProvider {
    /// Create a new Docker provider
    pub async fn new(socket_path: &str) -> Result<Self> {
        let client = if socket_path.starts_with("unix://") || socket_path.starts_with('/') {
            let path = socket_path.trim_start_matches("unix://");
            Docker::connect_with_socket(path, 120, bollard::API_DEFAULT_VERSION)
                .map_err(|e| ProviderError::ConnectionError(e.to_string()))?
        } else if socket_path.starts_with("http://") || socket_path.starts_with("https://") {
            Docker::connect_with_http(socket_path, 120, bollard::API_DEFAULT_VERSION)
                .map_err(|e| ProviderError::ConnectionError(e.to_string()))?
        } else {
            // Assume it's a unix socket path
            Docker::connect_with_socket(socket_path, 120, bollard::API_DEFAULT_VERSION)
                .map_err(|e| ProviderError::ConnectionError(e.to_string()))?
        };

        // Test connection
        client
            .ping()
            .await
            .map_err(|e| ProviderError::ConnectionError(e.to_string()))?;

        Ok(Self {
            client,
            provider_type: ProviderType::Docker,
        })
    }

    /// Create a new provider for Podman (uses Docker-compatible API)
    pub async fn new_podman(socket_path: &str) -> Result<Self> {
        let mut provider = Self::new(socket_path).await?;
        provider.provider_type = ProviderType::Podman;
        Ok(provider)
    }

    /// Get the underlying Docker client
    pub fn client(&self) -> &Docker {
        &self.client
    }
}

#[async_trait]
impl ContainerProvider for DockerProvider {
    async fn build(&self, config: &BuildConfig) -> Result<ImageId> {
        // Create a tarball of the build context
        let tar_data = create_build_context(&config.context, &config.dockerfile)?;

        let options = BuildImageOptions {
            dockerfile: config.dockerfile.clone(),
            t: config.tag.clone(),
            buildargs: config.build_args.clone(),
            nocache: config.no_cache,
            pull: config.pull,
            labels: config.labels.clone(),
            ..Default::default()
        };

        let mut stream = self.client.build_image(options, None, Some(tar_data.into()));

        let mut image_id = None;
        while let Some(result) = stream.next().await {
            match result {
                Ok(output) => {
                    if let Some(error) = output.error {
                        return Err(ProviderError::BuildError(error));
                    }
                    if let Some(aux) = output.aux {
                        if let Some(id) = aux.id {
                            image_id = Some(id);
                        }
                    }
                    if let Some(stream) = output.stream {
                        tracing::debug!("{}", stream.trim());
                    }
                }
                Err(e) => return Err(ProviderError::BuildError(e.to_string())),
            }
        }

        image_id
            .map(ImageId::new)
            .ok_or_else(|| ProviderError::BuildError("No image ID returned".to_string()))
    }

    async fn pull(&self, image: &str) -> Result<ImageId> {
        use bollard::image::CreateImageOptions;

        let options = CreateImageOptions {
            from_image: image,
            ..Default::default()
        };

        let mut stream = self.client.create_image(Some(options), None, None);

        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(error) = info.error {
                        return Err(ProviderError::ImageNotFound(error));
                    }
                    if let Some(status) = info.status {
                        tracing::debug!("{}", status);
                    }
                }
                Err(e) => return Err(ProviderError::RuntimeError(e.to_string())),
            }
        }

        // Get the image ID
        let inspect = self
            .client
            .inspect_image(image)
            .await
            .map_err(|e| ProviderError::ImageNotFound(e.to_string()))?;

        Ok(ImageId::new(inspect.id.unwrap_or_else(|| image.to_string())))
    }

    async fn create(&self, config: &CreateContainerConfig) -> Result<ContainerId> {
        let options = config.name.as_ref().map(|name| CreateContainerOptions {
            name: name.as_str(),
            platform: None,
        });

        // Build port bindings
        let mut port_bindings: HashMap<String, Option<Vec<PortBinding>>> = HashMap::new();
        let mut exposed_ports: HashMap<String, HashMap<(), ()>> = HashMap::new();

        for port in &config.ports {
            let container_port = format!("{}/{}", port.container_port, port.protocol);
            exposed_ports.insert(container_port.clone(), HashMap::new());

            let binding = PortBinding {
                host_ip: port.host_ip.clone(),
                host_port: port.host_port.map(|p| p.to_string()),
            };
            port_bindings.insert(container_port, Some(vec![binding]));
        }

        // Build mounts
        let mounts: Vec<Mount> = config
            .mounts
            .iter()
            .map(|m| Mount {
                target: Some(m.target.clone()),
                source: Some(m.source.clone()),
                typ: Some(match m.mount_type {
                    MountType::Bind => bollard::service::MountTypeEnum::BIND,
                    MountType::Volume => bollard::service::MountTypeEnum::VOLUME,
                    MountType::Tmpfs => bollard::service::MountTypeEnum::TMPFS,
                }),
                read_only: Some(m.read_only),
                ..Default::default()
            })
            .collect();

        let host_config = HostConfig {
            mounts: if mounts.is_empty() {
                None
            } else {
                Some(mounts)
            },
            port_bindings: if port_bindings.is_empty() {
                None
            } else {
                Some(port_bindings)
            },
            network_mode: config.network_mode.clone(),
            privileged: Some(config.privileged),
            cap_add: if config.cap_add.is_empty() {
                None
            } else {
                Some(config.cap_add.clone())
            },
            cap_drop: if config.cap_drop.is_empty() {
                None
            } else {
                Some(config.cap_drop.clone())
            },
            security_opt: if config.security_opt.is_empty() {
                None
            } else {
                Some(config.security_opt.clone())
            },
            ..Default::default()
        };

        let env: Vec<String> = config
            .env
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();

        let container_config = Config {
            image: Some(config.image.clone()),
            cmd: config.cmd.clone(),
            entrypoint: config.entrypoint.clone(),
            env: if env.is_empty() { None } else { Some(env) },
            working_dir: config.working_dir.clone(),
            user: config.user.clone(),
            hostname: config.hostname.clone(),
            tty: Some(config.tty),
            open_stdin: Some(config.stdin_open),
            labels: if config.labels.is_empty() {
                None
            } else {
                Some(config.labels.clone())
            },
            exposed_ports: if exposed_ports.is_empty() {
                None
            } else {
                Some(exposed_ports)
            },
            host_config: Some(host_config),
            ..Default::default()
        };

        let response = self
            .client
            .create_container(options, container_config)
            .await?;

        Ok(ContainerId::new(response.id))
    }

    async fn start(&self, id: &ContainerId) -> Result<()> {
        self.client
            .start_container(&id.0, None::<StartContainerOptions<String>>)
            .await?;
        Ok(())
    }

    async fn stop(&self, id: &ContainerId, timeout: Option<u32>) -> Result<()> {
        let options = StopContainerOptions {
            t: timeout.unwrap_or(10) as i64,
        };
        self.client.stop_container(&id.0, Some(options)).await?;
        Ok(())
    }

    async fn remove(&self, id: &ContainerId, force: bool) -> Result<()> {
        let options = RemoveContainerOptions {
            force,
            ..Default::default()
        };
        self.client.remove_container(&id.0, Some(options)).await?;
        Ok(())
    }

    async fn exec(&self, id: &ContainerId, config: &ExecConfig) -> Result<ExecResult> {
        let options = CreateExecOptions {
            cmd: Some(config.cmd.clone()),
            env: Some(
                config
                    .env
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect(),
            ),
            working_dir: config.working_dir.clone(),
            user: config.user.clone(),
            tty: Some(config.tty),
            attach_stdin: Some(config.stdin),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            privileged: Some(config.privileged),
            ..Default::default()
        };

        let exec = self.client.create_exec(&id.0, options).await?;

        let start_options = StartExecOptions {
            detach: false,
            tty: config.tty,
            ..Default::default()
        };

        let result = self.client.start_exec(&exec.id, Some(start_options)).await?;

        let mut output_str = String::new();

        match result {
            StartExecResults::Attached { mut output, .. } => {
                while let Some(chunk) = output.next().await {
                    match chunk {
                        Ok(bollard::container::LogOutput::StdOut { message }) => {
                            output_str.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(bollard::container::LogOutput::StdErr { message }) => {
                            output_str.push_str(&String::from_utf8_lossy(&message));
                        }
                        _ => {}
                    }
                }
            }
            StartExecResults::Detached => {}
        }

        // Get exit code
        let inspect = self.client.inspect_exec(&exec.id).await?;
        let exit_code = inspect.exit_code.unwrap_or(0);

        Ok(ExecResult { exit_code, output: output_str })
    }

    async fn exec_interactive(
        &self,
        id: &ContainerId,
        config: &ExecConfig,
    ) -> Result<ExecStream> {
        let options = CreateExecOptions {
            cmd: Some(config.cmd.clone()),
            env: Some(
                config
                    .env
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect(),
            ),
            working_dir: config.working_dir.clone(),
            user: config.user.clone(),
            tty: Some(config.tty),
            attach_stdin: Some(config.stdin),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            privileged: Some(config.privileged),
            ..Default::default()
        };

        let exec = self.client.create_exec(&id.0, options).await?;

        let start_options = StartExecOptions {
            detach: false,
            tty: config.tty,
            ..Default::default()
        };

        let result = self.client.start_exec(&exec.id, Some(start_options)).await?;

        match result {
            StartExecResults::Attached { output, input } => {
                // Create a reader that combines stdout/stderr
                let reader = LogOutputReader::new(output);

                Ok(ExecStream {
                    stdin: Some(Box::pin(input)),
                    output: Box::pin(reader),
                    id: exec.id,
                })
            }
            StartExecResults::Detached => Err(ProviderError::ExecError(
                "Exec started in detached mode".to_string(),
            )),
        }
    }

    async fn list(&self, all: bool) -> Result<Vec<ContainerInfo>> {
        let options = ListContainersOptions {
            all,
            filters: HashMap::from([("label".to_string(), vec!["devc.managed=true".to_string()])]),
            ..Default::default()
        };

        let containers = self.client.list_containers(Some(options)).await?;

        Ok(containers
            .into_iter()
            .map(|c| ContainerInfo {
                id: ContainerId::new(c.id.unwrap_or_default()),
                name: c
                    .names
                    .and_then(|n| n.first().cloned())
                    .unwrap_or_default()
                    .trim_start_matches('/')
                    .to_string(),
                image: c.image.unwrap_or_default(),
                status: c
                    .state
                    .as_deref()
                    .map(ContainerStatus::from)
                    .unwrap_or(ContainerStatus::Unknown),
                created: c.created.unwrap_or(0),
                labels: c.labels.unwrap_or_default(),
            })
            .collect())
    }

    async fn inspect(&self, id: &ContainerId) -> Result<ContainerDetails> {
        let info = self.client.inspect_container(&id.0, None).await?;

        let state = info.state.as_ref();
        let status = state
            .and_then(|s| s.status)
            .map(|s| ContainerStatus::from(format!("{:?}", s).to_lowercase().as_str()))
            .unwrap_or(ContainerStatus::Unknown);

        let config = info.config.as_ref();
        let _host_config = info.host_config.as_ref();

        // Parse mounts
        let mounts = info
            .mounts
            .unwrap_or_default()
            .into_iter()
            .map(|m| MountInfo {
                mount_type: m
                    .typ
                    .map(|t| format!("{:?}", t).to_lowercase())
                    .unwrap_or_else(|| "unknown".to_string()),
                source: m.source.unwrap_or_default(),
                destination: m.destination.unwrap_or_default(),
                read_only: m.rw.map(|rw| !rw).unwrap_or(false),
            })
            .collect();

        // Parse ports
        let mut ports = Vec::new();
        if let Some(network) = &info.network_settings {
            if let Some(port_map) = &network.ports {
                for (container_port, bindings) in port_map {
                    let parts: Vec<&str> = container_port.split('/').collect();
                    let port_num: u16 = parts[0].parse().unwrap_or(0);
                    let protocol = parts.get(1).unwrap_or(&"tcp").to_string();

                    if let Some(bindings) = bindings {
                        for binding in bindings {
                            ports.push(PortInfo {
                                container_port: port_num,
                                host_port: binding.host_port.as_ref().and_then(|p| p.parse().ok()),
                                protocol: protocol.clone(),
                                host_ip: binding.host_ip.clone(),
                            });
                        }
                    } else {
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
        let network_settings = info
            .network_settings
            .as_ref()
            .map(|ns| NetworkSettings {
                ip_address: ns.ip_address.clone(),
                gateway: ns.gateway.clone(),
                networks: ns
                    .networks
                    .as_ref()
                    .map(|nets| {
                        nets.iter()
                            .map(|(name, net)| {
                                (
                                    name.clone(),
                                    NetworkInfo {
                                        network_id: net.network_id.clone().unwrap_or_default(),
                                        ip_address: net.ip_address.clone(),
                                        gateway: net.gateway.clone(),
                                    },
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .unwrap_or_default();

        // Parse timestamps
        let started_at = state
            .and_then(|s| s.started_at.as_ref())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp());

        let finished_at = state
            .and_then(|s| s.finished_at.as_ref())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.timestamp());

        Ok(ContainerDetails {
            id: id.clone(),
            name: info
                .name
                .unwrap_or_default()
                .trim_start_matches('/')
                .to_string(),
            image: config
                .and_then(|c| c.image.clone())
                .unwrap_or_default(),
            image_id: info.image.unwrap_or_default(),
            status,
            created: info
                .created
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.timestamp())
                .unwrap_or(0),
            started_at,
            finished_at,
            exit_code: state.and_then(|s| s.exit_code),
            labels: config.and_then(|c| c.labels.clone()).unwrap_or_default(),
            env: config.and_then(|c| c.env.clone()).unwrap_or_default(),
            mounts,
            ports,
            network_settings,
        })
    }

    async fn logs(&self, id: &ContainerId, config: &LogConfig) -> Result<LogStream> {
        let options = LogsOptions {
            follow: config.follow,
            stdout: config.stdout,
            stderr: config.stderr,
            tail: config.tail.map(|t| t.to_string()).unwrap_or_else(|| "all".to_string()),
            timestamps: config.timestamps,
            since: config.since.unwrap_or(0),
            until: config.until.unwrap_or(0),
        };

        let stream = self.client.logs(&id.0, Some(options));
        let reader = LogOutputReader::new(stream);

        Ok(LogStream {
            stream: Box::pin(reader),
        })
    }

    async fn ping(&self) -> Result<()> {
        self.client
            .ping()
            .await
            .map_err(|e| ProviderError::ConnectionError(e.to_string()))?;
        Ok(())
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            provider_type: self.provider_type,
            version: "unknown".to_string(),
            api_version: bollard::API_DEFAULT_VERSION.to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        }
    }

    async fn copy_into(
        &self,
        id: &ContainerId,
        src: &Path,
        dest: &str,
    ) -> Result<()> {
        use bollard::container::UploadToContainerOptions;

        // Create tar archive of the source
        let tar_data = create_tar_from_path(src)?;

        let options = UploadToContainerOptions {
            path: dest,
            ..Default::default()
        };

        self.client
            .upload_to_container(&id.0, Some(options), tar_data.into())
            .await?;

        Ok(())
    }

    async fn copy_from(
        &self,
        id: &ContainerId,
        src: &str,
        dest: &Path,
    ) -> Result<()> {
        use bollard::container::DownloadFromContainerOptions;

        let options = DownloadFromContainerOptions { path: src };

        let mut stream = self.client.download_from_container(&id.0, Some(options));

        // Collect all chunks
        let mut tar_data = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            tar_data.extend_from_slice(&chunk);
        }

        // Extract tar archive
        extract_tar_to_path(&tar_data, dest)?;

        Ok(())
    }
}

/// Create a tar archive from the build context
fn create_build_context(context: &Path, dockerfile: &str) -> Result<Vec<u8>> {
    use std::io::Cursor;
    use tar::Builder;

    let mut tar_data = Vec::new();
    {
        let cursor = Cursor::new(&mut tar_data);
        let mut builder = Builder::new(cursor);

        // Add Dockerfile
        let dockerfile_path = context.join(dockerfile);
        if dockerfile_path.exists() {
            builder
                .append_path_with_name(&dockerfile_path, dockerfile)
                .map_err(|e| ProviderError::IoError(e))?;
        }

        // Add all files in context
        add_dir_to_tar(&mut builder, context, Path::new(""))?;

        builder.finish().map_err(|e| ProviderError::IoError(e))?;
    }

    Ok(tar_data)
}

/// Recursively add directory contents to tar
fn add_dir_to_tar<W: Write>(
    builder: &mut tar::Builder<W>,
    base: &Path,
    prefix: &Path,
) -> Result<()> {
    let entries = std::fs::read_dir(base).map_err(|e| ProviderError::IoError(e))?;

    for entry in entries {
        let entry = entry.map_err(|e| ProviderError::IoError(e))?;
        let path = entry.path();
        let name = prefix.join(entry.file_name());

        // Skip common excludes
        let file_name = entry.file_name();
        let file_name_str = file_name.to_string_lossy();
        if file_name_str == ".git"
            || file_name_str == "node_modules"
            || file_name_str == "target"
            || file_name_str == ".dockerignore"
        {
            continue;
        }

        if path.is_dir() {
            add_dir_to_tar(builder, &path, &name)?;
        } else if path.is_file() {
            builder
                .append_path_with_name(&path, &name)
                .map_err(|e| ProviderError::IoError(e))?;
        }
    }

    Ok(())
}

/// Create a tar archive from a single path
fn create_tar_from_path(path: &Path) -> Result<Vec<u8>> {
    use std::io::Cursor;
    use tar::Builder;

    let mut tar_data = Vec::new();
    {
        let cursor = Cursor::new(&mut tar_data);
        let mut builder = Builder::new(cursor);

        if path.is_file() {
            builder
                .append_path_with_name(path, path.file_name().unwrap_or_default())
                .map_err(|e| ProviderError::IoError(e))?;
        } else if path.is_dir() {
            add_dir_to_tar(&mut builder, path, Path::new(""))?;
        }

        builder.finish().map_err(|e| ProviderError::IoError(e))?;
    }

    Ok(tar_data)
}

/// Extract a tar archive to a path
fn extract_tar_to_path(tar_data: &[u8], dest: &Path) -> Result<()> {
    use std::io::Cursor;
    use tar::Archive;

    let cursor = Cursor::new(tar_data);
    let mut archive = Archive::new(cursor);

    archive
        .unpack(dest)
        .map_err(|e| ProviderError::IoError(e))?;

    Ok(())
}

/// Reader that converts log output stream to AsyncRead
struct LogOutputReader<S> {
    stream: S,
    buffer: Vec<u8>,
    pos: usize,
}

impl<S> LogOutputReader<S> {
    fn new(stream: S) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
            pos: 0,
        }
    }
}

impl<S> AsyncRead for LogOutputReader<S>
where
    S: futures::Stream<Item = std::result::Result<bollard::container::LogOutput, bollard::errors::Error>>
        + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        // If we have buffered data, return it first
        if self.pos < self.buffer.len() {
            let remaining = &self.buffer[self.pos..];
            let to_copy = std::cmp::min(remaining.len(), buf.remaining());
            buf.put_slice(&remaining[..to_copy]);
            self.pos += to_copy;
            return std::task::Poll::Ready(Ok(()));
        }

        // Clear buffer and try to get more data
        self.buffer.clear();
        self.pos = 0;

        match Pin::new(&mut self.stream).poll_next(cx) {
            std::task::Poll::Ready(Some(Ok(output))) => {
                let data = match output {
                    bollard::container::LogOutput::StdOut { message } => message,
                    bollard::container::LogOutput::StdErr { message } => message,
                    bollard::container::LogOutput::StdIn { message } => message,
                    bollard::container::LogOutput::Console { message } => message,
                };
                self.buffer = data.to_vec();

                let to_copy = std::cmp::min(self.buffer.len(), buf.remaining());
                buf.put_slice(&self.buffer[..to_copy]);
                self.pos = to_copy;
                std::task::Poll::Ready(Ok(()))
            }
            std::task::Poll::Ready(Some(Err(e))) => {
                std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    e.to_string(),
                )))
            }
            std::task::Poll::Ready(None) => std::task::Poll::Ready(Ok(())),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}
