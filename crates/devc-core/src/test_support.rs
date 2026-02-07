//! Test support utilities for devc-core
//!
//! Provides MockProvider and helpers for unit testing the ContainerManager
//! without requiring a real Docker/Podman runtime.

use async_trait::async_trait;
use devc_provider::*;
use std::collections::HashMap;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

/// Records which methods were called on the mock
#[derive(Debug, Clone, PartialEq)]
pub enum MockCall {
    Build { tag: String },
    BuildWithProgress { tag: String },
    Pull { image: String },
    Create { image: String, name: Option<String> },
    Start { id: String },
    Stop { id: String },
    Remove { id: String, force: bool },
    RemoveByName { name: String },
    Exec { id: String, cmd: Vec<String> },
    ExecInteractive { id: String },
    Inspect { id: String },
    List { all: bool },
    Logs { id: String },
    Ping,
    Discover,
    CopyInto { id: String, dest: String },
    CopyFrom { id: String, src: String },
}

/// Configurable mock container provider for testing
pub struct MockProvider {
    pub provider_type: ProviderType,
    pub calls: Arc<Mutex<Vec<MockCall>>>,
    /// Result for build calls
    pub build_result: Arc<Mutex<Result<ImageId>>>,
    /// Result for pull calls
    pub pull_result: Arc<Mutex<Result<ImageId>>>,
    /// Result for create calls
    pub create_result: Arc<Mutex<Result<ContainerId>>>,
    /// Result for start calls
    pub start_result: Arc<Mutex<Result<()>>>,
    /// Result for stop calls
    pub stop_result: Arc<Mutex<Result<()>>>,
    /// Result for remove calls
    pub remove_result: Arc<Mutex<Result<()>>>,
    /// Result for remove_by_name calls
    pub remove_by_name_result: Arc<Mutex<Result<()>>>,
    /// Exit code and output for exec calls
    pub exec_exit_code: Arc<Mutex<i64>>,
    pub exec_output: Arc<Mutex<String>>,
    /// Error for exec calls (if Some, exec returns this error)
    pub exec_error: Arc<Mutex<Option<ProviderError>>>,
    /// Result for inspect calls
    pub inspect_result: Arc<Mutex<Result<ContainerDetails>>>,
    /// Result for list calls
    pub list_result: Arc<Mutex<Result<Vec<ContainerInfo>>>>,
    /// Result for ping calls
    pub ping_result: Arc<Mutex<Result<()>>>,
    /// Result for discover calls
    pub discover_result: Arc<Mutex<Result<Vec<DiscoveredContainer>>>>,
    /// Result for copy_into calls
    pub copy_into_result: Arc<Mutex<Result<()>>>,
    /// Result for copy_from calls
    pub copy_from_result: Arc<Mutex<Result<()>>>,
}

impl MockProvider {
    /// Create a new mock provider with default success results
    pub fn new(provider_type: ProviderType) -> Self {
        Self {
            provider_type,
            calls: Arc::new(Mutex::new(Vec::new())),
            build_result: Arc::new(Mutex::new(Ok(ImageId::new("sha256:mock_image_id")))),
            pull_result: Arc::new(Mutex::new(Ok(ImageId::new("sha256:mock_pulled_id")))),
            create_result: Arc::new(Mutex::new(Ok(ContainerId::new("mock_container_id")))),
            start_result: Arc::new(Mutex::new(Ok(()))),
            stop_result: Arc::new(Mutex::new(Ok(()))),
            remove_result: Arc::new(Mutex::new(Ok(()))),
            remove_by_name_result: Arc::new(Mutex::new(Ok(()))),
            exec_exit_code: Arc::new(Mutex::new(0)),
            exec_output: Arc::new(Mutex::new(String::new())),
            exec_error: Arc::new(Mutex::new(None)),
            inspect_result: Arc::new(Mutex::new(Ok(mock_container_details(
                "mock_container_id",
                ContainerStatus::Running,
            )))),
            list_result: Arc::new(Mutex::new(Ok(Vec::new()))),
            ping_result: Arc::new(Mutex::new(Ok(()))),
            discover_result: Arc::new(Mutex::new(Ok(Vec::new()))),
            copy_into_result: Arc::new(Mutex::new(Ok(()))),
            copy_from_result: Arc::new(Mutex::new(Ok(()))),
        }
    }

    /// Record a call
    fn record(&self, call: MockCall) {
        self.calls.lock().unwrap().push(call);
    }

    /// Get all recorded calls
    pub fn get_calls(&self) -> Vec<MockCall> {
        self.calls.lock().unwrap().clone()
    }

    /// Check if a specific call was made
    pub fn was_called(&self, call: &MockCall) -> bool {
        self.calls.lock().unwrap().contains(call)
    }
}

/// Helper to clone a Result<T> from an Arc<Mutex<Result<T>>>
fn clone_result<T: Clone>(r: &Arc<Mutex<Result<T>>>) -> Result<T> {
    let guard = r.lock().unwrap();
    match &*guard {
        Ok(v) => Ok(v.clone()),
        Err(e) => Err(clone_provider_error(e)),
    }
}

/// Clone a ProviderError (thiserror types don't implement Clone)
fn clone_provider_error(e: &ProviderError) -> ProviderError {
    match e {
        ProviderError::ConnectionError(s) => ProviderError::ConnectionError(s.clone()),
        ProviderError::ContainerNotFound(s) => ProviderError::ContainerNotFound(s.clone()),
        ProviderError::ImageNotFound(s) => ProviderError::ImageNotFound(s.clone()),
        ProviderError::BuildError(s) => ProviderError::BuildError(s.clone()),
        ProviderError::ExecError(s) => ProviderError::ExecError(s.clone()),
        ProviderError::RuntimeError(s) => ProviderError::RuntimeError(s.clone()),
        ProviderError::ConfigError(s) => ProviderError::ConfigError(s.clone()),
        ProviderError::Unsupported(s) => ProviderError::Unsupported(s.clone()),
        ProviderError::Timeout => ProviderError::Timeout,
        ProviderError::Cancelled => ProviderError::Cancelled,
        ProviderError::IoError(_) => ProviderError::RuntimeError("IO error (cloned)".into()),
    }
}

/// Create a mock ContainerDetails
pub fn mock_container_details(id: &str, status: ContainerStatus) -> ContainerDetails {
    ContainerDetails {
        id: ContainerId::new(id),
        name: "mock_container".to_string(),
        image: "mock_image:latest".to_string(),
        image_id: "sha256:mock_image_id".to_string(),
        status,
        created: 0,
        started_at: None,
        finished_at: None,
        exit_code: None,
        labels: HashMap::new(),
        env: Vec::new(),
        mounts: Vec::new(),
        ports: Vec::new(),
        network_settings: NetworkSettings::default(),
    }
}

/// A no-op async reader for mock ExecStream
struct EmptyReader;

impl AsyncRead for EmptyReader {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        _buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

/// A no-op async writer for mock ExecStream
struct EmptyWriter;

impl AsyncWrite for EmptyWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[async_trait]
impl ContainerProvider for MockProvider {
    async fn build(&self, config: &BuildConfig) -> Result<ImageId> {
        self.record(MockCall::Build {
            tag: config.tag.clone(),
        });
        clone_result(&self.build_result)
    }

    async fn build_with_progress(
        &self,
        config: &BuildConfig,
        _progress: mpsc::UnboundedSender<String>,
    ) -> Result<ImageId> {
        self.record(MockCall::BuildWithProgress {
            tag: config.tag.clone(),
        });
        clone_result(&self.build_result)
    }

    async fn pull(&self, image: &str) -> Result<ImageId> {
        self.record(MockCall::Pull {
            image: image.to_string(),
        });
        clone_result(&self.pull_result)
    }

    async fn create(&self, config: &CreateContainerConfig) -> Result<ContainerId> {
        self.record(MockCall::Create {
            image: config.image.clone(),
            name: config.name.clone(),
        });
        clone_result(&self.create_result)
    }

    async fn start(&self, id: &ContainerId) -> Result<()> {
        self.record(MockCall::Start { id: id.0.clone() });
        clone_result(&self.start_result)
    }

    async fn stop(&self, id: &ContainerId, _timeout: Option<u32>) -> Result<()> {
        self.record(MockCall::Stop { id: id.0.clone() });
        clone_result(&self.stop_result)
    }

    async fn remove(&self, id: &ContainerId, force: bool) -> Result<()> {
        self.record(MockCall::Remove {
            id: id.0.clone(),
            force,
        });
        clone_result(&self.remove_result)
    }

    async fn remove_by_name(&self, name: &str) -> Result<()> {
        self.record(MockCall::RemoveByName {
            name: name.to_string(),
        });
        clone_result(&self.remove_by_name_result)
    }

    async fn exec(&self, id: &ContainerId, config: &ExecConfig) -> Result<ExecResult> {
        self.record(MockCall::Exec {
            id: id.0.clone(),
            cmd: config.cmd.clone(),
        });
        if let Some(err) = self.exec_error.lock().unwrap().as_ref() {
            return Err(clone_provider_error(err));
        }
        Ok(ExecResult {
            exit_code: *self.exec_exit_code.lock().unwrap(),
            output: self.exec_output.lock().unwrap().clone(),
        })
    }

    async fn exec_interactive(
        &self,
        id: &ContainerId,
        _config: &ExecConfig,
    ) -> Result<ExecStream> {
        self.record(MockCall::ExecInteractive { id: id.0.clone() });
        Ok(ExecStream {
            stdin: Some(Box::pin(EmptyWriter)),
            output: Box::pin(EmptyReader),
            id: id.0.clone(),
        })
    }

    async fn list(&self, all: bool) -> Result<Vec<ContainerInfo>> {
        self.record(MockCall::List { all });
        clone_result(&self.list_result)
    }

    async fn inspect(&self, id: &ContainerId) -> Result<ContainerDetails> {
        self.record(MockCall::Inspect { id: id.0.clone() });
        clone_result(&self.inspect_result)
    }

    async fn logs(&self, id: &ContainerId, _config: &LogConfig) -> Result<LogStream> {
        self.record(MockCall::Logs { id: id.0.clone() });
        Ok(LogStream {
            stream: Box::pin(EmptyReader),
            _child: None,
        })
    }

    async fn ping(&self) -> Result<()> {
        self.record(MockCall::Ping);
        clone_result(&self.ping_result)
    }

    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            provider_type: self.provider_type,
            version: "mock-1.0".to_string(),
            api_version: "mock".to_string(),
            os: "test".to_string(),
            arch: "test".to_string(),
        }
    }

    async fn discover_devcontainers(&self) -> Result<Vec<DiscoveredContainer>> {
        self.record(MockCall::Discover);
        clone_result(&self.discover_result)
    }

    async fn copy_into(&self, id: &ContainerId, _src: &Path, dest: &str) -> Result<()> {
        self.record(MockCall::CopyInto {
            id: id.0.clone(),
            dest: dest.to_string(),
        });
        clone_result(&self.copy_into_result)
    }

    async fn copy_from(&self, id: &ContainerId, src: &str, _dest: &Path) -> Result<()> {
        self.record(MockCall::CopyFrom {
            id: id.0.clone(),
            src: src.to_string(),
        });
        clone_result(&self.copy_from_result)
    }
}
