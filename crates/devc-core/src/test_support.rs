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
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

/// Records which methods were called on the mock
#[derive(Debug, Clone, PartialEq)]
pub enum MockCall {
    Build {
        tag: String,
    },
    BuildWithProgress {
        tag: String,
    },
    Pull {
        image: String,
    },
    Create {
        image: String,
        name: Option<String>,
    },
    Start {
        id: String,
    },
    Stop {
        id: String,
    },
    Remove {
        id: String,
        force: bool,
    },
    RemoveByName {
        name: String,
    },
    Exec {
        id: String,
        cmd: Vec<String>,
        working_dir: Option<String>,
        user: Option<String>,
    },
    ExecInteractive {
        id: String,
    },
    Inspect {
        id: String,
    },
    List {
        all: bool,
    },
    Logs {
        id: String,
    },
    Ping,
    ComposeUp {
        project: String,
    },
    ComposeDown {
        project: String,
    },
    ComposePs {
        project: String,
    },
    ComposeResolveServiceId {
        project: String,
        service: String,
    },
    Discover,
    CopyInto {
        id: String,
        dest: String,
    },
    CopyFrom {
        id: String,
        src: String,
    },
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
    /// Per-call exec response queue: (exit_code, output). Popped before falling back to exec_exit_code/exec_output.
    pub exec_responses: Arc<Mutex<Vec<(i64, String)>>>,
    /// Result for inspect calls
    pub inspect_result: Arc<Mutex<Result<ContainerDetails>>>,
    /// Per-call inspect response queue. Popped before falling back to inspect_result.
    pub inspect_responses: Arc<Mutex<Vec<Result<ContainerDetails>>>>,
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
    /// Result for compose_up calls
    pub compose_up_result: Arc<Mutex<Result<()>>>,
    /// Result for compose_down calls
    pub compose_down_result: Arc<Mutex<Result<()>>>,
    /// Result for compose_ps calls
    pub compose_ps_result: Arc<Mutex<Result<Vec<ComposeServiceInfo>>>>,
    /// Result for compose_resolve_service_id calls
    pub compose_resolve_service_id_result: Arc<Mutex<Result<ContainerId>>>,
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
            exec_responses: Arc::new(Mutex::new(Vec::new())),
            inspect_responses: Arc::new(Mutex::new(Vec::new())),
            compose_up_result: Arc::new(Mutex::new(Ok(()))),
            compose_down_result: Arc::new(Mutex::new(Ok(()))),
            compose_ps_result: Arc::new(Mutex::new(Ok(Vec::new()))),
            compose_resolve_service_id_result: Arc::new(Mutex::new(Ok(ContainerId::new(
                "mock_compose_service_id",
            )))),
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

    /// Count calls matching a predicate
    pub fn call_count<F: Fn(&MockCall) -> bool>(&self, filter: F) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| filter(c))
            .count()
    }

    /// Get all exec command vecs (convenience)
    pub fn exec_commands(&self) -> Vec<Vec<String>> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter_map(|c| match c {
                MockCall::Exec { cmd, .. } => Some(cmd.clone()),
                _ => None,
            })
            .collect()
    }

    /// Assert that calls were made in a specific order (by variant name prefix).
    /// Example: `mock.assert_call_order(&["Build", "Create", "Start", "Exec"])`
    pub fn assert_call_order(&self, expected: &[&str]) {
        let calls = self.calls.lock().unwrap();
        let actual_names: Vec<String> = calls.iter().map(mock_call_name).collect();

        let mut expected_idx = 0;
        for name in &actual_names {
            if expected_idx < expected.len() && name == expected[expected_idx] {
                expected_idx += 1;
            }
        }

        assert_eq!(
            expected_idx,
            expected.len(),
            "Expected call order {:?} but got calls: {:?}",
            expected,
            actual_names,
        );
    }
}

/// Get the variant name of a MockCall (for assertion helpers)
fn mock_call_name(call: &MockCall) -> String {
    match call {
        MockCall::Build { .. } => "Build",
        MockCall::BuildWithProgress { .. } => "BuildWithProgress",
        MockCall::Pull { .. } => "Pull",
        MockCall::Create { .. } => "Create",
        MockCall::Start { .. } => "Start",
        MockCall::Stop { .. } => "Stop",
        MockCall::Remove { .. } => "Remove",
        MockCall::RemoveByName { .. } => "RemoveByName",
        MockCall::Exec { .. } => "Exec",
        MockCall::ExecInteractive { .. } => "ExecInteractive",
        MockCall::Inspect { .. } => "Inspect",
        MockCall::List { .. } => "List",
        MockCall::Logs { .. } => "Logs",
        MockCall::Ping => "Ping",
        MockCall::ComposeUp { .. } => "ComposeUp",
        MockCall::ComposeDown { .. } => "ComposeDown",
        MockCall::ComposePs { .. } => "ComposePs",
        MockCall::ComposeResolveServiceId { .. } => "ComposeResolveServiceId",
        MockCall::Discover => "Discover",
        MockCall::CopyInto { .. } => "CopyInto",
        MockCall::CopyFrom { .. } => "CopyFrom",
    }
    .to_string()
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
            working_dir: config.working_dir.clone(),
            user: config.user.clone(),
        });
        if let Some(err) = self.exec_error.lock().unwrap().as_ref() {
            return Err(clone_provider_error(err));
        }
        // Pop from queue if available, otherwise fall back to single-value fields
        let mut queue = self.exec_responses.lock().unwrap();
        if !queue.is_empty() {
            let (exit_code, output) = queue.remove(0);
            return Ok(ExecResult { exit_code, output });
        }
        drop(queue);
        Ok(ExecResult {
            exit_code: *self.exec_exit_code.lock().unwrap(),
            output: self.exec_output.lock().unwrap().clone(),
        })
    }

    async fn exec_interactive(&self, id: &ContainerId, _config: &ExecConfig) -> Result<ExecStream> {
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
        // Pop from queue if available, otherwise fall back to single-value field
        let mut queue = self.inspect_responses.lock().unwrap();
        if !queue.is_empty() {
            return match queue.remove(0) {
                Ok(v) => Ok(v),
                Err(e) => Err(clone_provider_error(&e)),
            };
        }
        drop(queue);
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

    fn runtime_args(&self) -> (String, Vec<String>) {
        (self.provider_type.to_string(), vec![])
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

    async fn compose_up(
        &self,
        _compose_files: &[&str],
        project_name: &str,
        _project_dir: &Path,
        _progress: Option<mpsc::UnboundedSender<String>>,
    ) -> Result<()> {
        self.record(MockCall::ComposeUp {
            project: project_name.to_string(),
        });
        clone_result(&self.compose_up_result)
    }

    async fn compose_down(
        &self,
        _compose_files: &[&str],
        project_name: &str,
        _project_dir: &Path,
    ) -> Result<()> {
        self.record(MockCall::ComposeDown {
            project: project_name.to_string(),
        });
        clone_result(&self.compose_down_result)
    }

    async fn compose_ps(
        &self,
        _compose_files: &[&str],
        project_name: &str,
        _project_dir: &Path,
    ) -> Result<Vec<ComposeServiceInfo>> {
        self.record(MockCall::ComposePs {
            project: project_name.to_string(),
        });
        clone_result(&self.compose_ps_result)
    }

    async fn compose_resolve_service_id(
        &self,
        _compose_files: &[&str],
        project_name: &str,
        _project_dir: &Path,
        service_name: &str,
        _timeout: Duration,
    ) -> Result<ContainerId> {
        self.record(MockCall::ComposeResolveServiceId {
            project: project_name.to_string(),
            service: service_name.to_string(),
        });
        clone_result(&self.compose_resolve_service_id_result)
    }
}

// ============================================================================
// RAII guards for panic-safe container cleanup in E2E tests
// ============================================================================

use std::sync::atomic::{AtomicBool, Ordering};

/// RAII guard for a single container created during an E2E test.
///
/// On drop (including panics), forcibly removes the container, its image, and
/// any associated volumes via sync `std::process::Command`.
pub struct TestContainerGuard {
    runtime: String,
    prefix: Vec<String>,
    container_id: String,
    container_name: Option<String>,
    image: Option<String>,
    volumes: Vec<String>,
    cleaned: AtomicBool,
}

impl TestContainerGuard {
    /// Create a guard for the given container.
    ///
    /// `runtime` and `prefix` come from `provider.runtime_args()`.
    pub fn new(runtime: String, prefix: Vec<String>, container_id: String) -> Self {
        Self {
            runtime,
            prefix,
            container_id,
            container_name: None,
            image: None,
            volumes: Vec::new(),
            cleaned: AtomicBool::new(false),
        }
    }

    /// Also track the container name (for cleanup by name as a fallback).
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.container_name = Some(name.into());
        self
    }

    /// Also remove this image on cleanup.
    pub fn with_image(mut self, tag: impl Into<String>) -> Self {
        self.image = Some(tag.into());
        self
    }

    /// Also remove this volume on cleanup.
    pub fn with_volume(mut self, name: impl Into<String>) -> Self {
        self.volumes.push(name.into());
        self
    }

    /// Explicit async cleanup via the provider. Call at the end of a test's
    /// happy path. Sets the `cleaned` flag so Drop is a no-op.
    pub async fn cleanup(&self, provider: &dyn ContainerProvider) {
        let cid = ContainerId::new(&self.container_id);
        let _ = provider.remove(&cid, true).await;
        self.cleaned.store(true, Ordering::SeqCst);
    }

    /// Mark as already cleaned so Drop is a no-op.
    pub fn mark_cleaned(&self) {
        self.cleaned.store(true, Ordering::SeqCst);
    }

    fn build_sync_cmd(&self) -> std::process::Command {
        if self.prefix.is_empty() {
            std::process::Command::new(&self.runtime)
        } else {
            let mut c = std::process::Command::new(&self.prefix[0]);
            for arg in &self.prefix[1..] {
                c.arg(arg);
            }
            c.arg(&self.runtime);
            c
        }
    }
}

impl Drop for TestContainerGuard {
    fn drop(&mut self) {
        if self.cleaned.load(Ordering::SeqCst) {
            return;
        }

        // Remove container by ID
        let _ = self
            .build_sync_cmd()
            .args(["rm", "-f", &self.container_id])
            .output();

        // Remove container by name as fallback
        if let Some(ref name) = self.container_name {
            let _ = self.build_sync_cmd().args(["rm", "-f", name]).output();
        }

        // Remove image
        if let Some(ref image) = self.image {
            let _ = self.build_sync_cmd().args(["rmi", image]).output();
        }

        // Remove volumes
        for vol in &self.volumes {
            let _ = self.build_sync_cmd().args(["volume", "rm", vol]).output();
        }
    }
}

/// RAII guard for a Docker Compose project created during an E2E test.
///
/// On drop (including panics), runs `compose down --remove-orphans` via sync
/// `std::process::Command`.
pub struct TestComposeGuard {
    runtime: String,
    prefix: Vec<String>,
    compose_files: Vec<String>,
    project_name: String,
    project_dir: std::path::PathBuf,
    cleaned: AtomicBool,
}

impl TestComposeGuard {
    /// Create a guard for the given compose project.
    pub fn new(
        runtime: String,
        prefix: Vec<String>,
        compose_files: Vec<String>,
        project_name: String,
        project_dir: std::path::PathBuf,
    ) -> Self {
        Self {
            runtime,
            prefix,
            compose_files,
            project_name,
            project_dir,
            cleaned: AtomicBool::new(false),
        }
    }

    /// Explicit async cleanup via the provider. Sets the `cleaned` flag.
    pub async fn cleanup(&self, provider: &dyn ContainerProvider) {
        let file_strs: Vec<&str> = self.compose_files.iter().map(|s| s.as_str()).collect();
        let _ = provider
            .compose_down(&file_strs, &self.project_name, &self.project_dir)
            .await;
        self.cleaned.store(true, Ordering::SeqCst);
    }

    /// Mark as already cleaned so Drop is a no-op.
    pub fn mark_cleaned(&self) {
        self.cleaned.store(true, Ordering::SeqCst);
    }

    fn build_sync_cmd(&self) -> std::process::Command {
        if self.prefix.is_empty() {
            std::process::Command::new(&self.runtime)
        } else {
            let mut c = std::process::Command::new(&self.prefix[0]);
            for arg in &self.prefix[1..] {
                c.arg(arg);
            }
            c.arg(&self.runtime);
            c
        }
    }
}

impl Drop for TestComposeGuard {
    fn drop(&mut self) {
        if self.cleaned.load(Ordering::SeqCst) {
            return;
        }

        let mut cmd = self.build_sync_cmd();
        cmd.arg("compose");
        for f in &self.compose_files {
            cmd.arg("-f").arg(f);
        }
        cmd.args(["-p", &self.project_name, "down", "--remove-orphans"]);
        cmd.current_dir(&self.project_dir);
        let _ = cmd.output();
    }
}
