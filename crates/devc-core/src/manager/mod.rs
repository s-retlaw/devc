//! Container manager - coordinates all container operations

mod build;
mod compose;
mod discovery;
mod exec;
mod lifecycle;

use crate::{
    run_feature_lifecycle_commands, run_lifecycle_command_with_env, Container, ContainerState,
    CoreError, DevcContainerStatus, Result, StateStore,
};
use devc_config::GlobalConfig;
use devc_provider::{
    ContainerId, ContainerProvider, ContainerStatus, DevcontainerSource,
    LogConfig, ProviderType,
};
use crate::features;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;

/// Main container manager
pub struct ContainerManager {
    /// Available container providers, keyed by type
    providers: HashMap<ProviderType, Box<dyn ContainerProvider>>,
    /// Default provider type for new containers (None if fully disconnected)
    default_provider_type: Option<ProviderType>,
    /// State store
    state: Arc<RwLock<StateStore>>,
    /// Global configuration
    global_config: GlobalConfig,
    /// Error message when disconnected
    connection_error: Option<String>,
}

/// Context prepared by `prepare_exec()` for exec/shell operations.
pub(crate) struct ExecContext<'a> {
    pub(crate) provider: &'a dyn ContainerProvider,
    pub(crate) container_state: ContainerState,
    pub(crate) cid: ContainerId,
    pub(crate) feature_props: features::MergedFeatureProperties,
}

impl ContainerManager {
    /// Create a new container manager
    pub async fn new(provider: Box<dyn ContainerProvider>) -> Result<Self> {
        let global_config = GlobalConfig::load()?;
        let state = StateStore::load()?;
        let default_type = provider.info().provider_type;

        let mut providers = HashMap::new();
        providers.insert(default_type, provider);

        // Try to also cache the other provider type for cross-provider operations
        for &pt in &[ProviderType::Docker, ProviderType::Podman] {
            if pt != default_type {
                if let Ok(p) = devc_provider::create_provider(pt, &global_config).await {
                    providers.insert(pt, p);
                }
            }
        }

        Ok(Self {
            providers,
            default_provider_type: Some(default_type),
            state: Arc::new(RwLock::new(state)),
            global_config,
            connection_error: None,
        })
    }

    /// Create with specific global config
    pub async fn with_config(
        provider: Box<dyn ContainerProvider>,
        global_config: GlobalConfig,
    ) -> Result<Self> {
        let state = StateStore::load()?;
        let default_type = provider.info().provider_type;

        let mut providers = HashMap::new();
        providers.insert(default_type, provider);

        // Try to also cache the other provider type for cross-provider operations
        for &pt in &[ProviderType::Docker, ProviderType::Podman] {
            if pt != default_type {
                if let Ok(p) = devc_provider::create_provider(pt, &global_config).await {
                    providers.insert(pt, p);
                }
            }
        }

        Ok(Self {
            providers,
            default_provider_type: Some(default_type),
            state: Arc::new(RwLock::new(state)),
            global_config,
            connection_error: None,
        })
    }

    /// Create a manager for testing with injectable dependencies
    #[cfg(any(test, feature = "test-support"))]
    pub fn new_for_testing(
        provider: Box<dyn ContainerProvider>,
        global_config: GlobalConfig,
        state: StateStore,
    ) -> Self {
        let pt = provider.info().provider_type;
        let mut providers = HashMap::new();
        providers.insert(pt, provider);
        Self {
            providers,
            default_provider_type: Some(pt),
            state: Arc::new(RwLock::new(state)),
            global_config,
            connection_error: None,
        }
    }

    /// Create a manager with multiple providers for testing cross-provider operations
    #[cfg(any(test, feature = "test-support"))]
    pub fn new_for_testing_multi(
        providers_list: Vec<Box<dyn ContainerProvider>>,
        default_type: ProviderType,
        global_config: GlobalConfig,
        state: StateStore,
    ) -> Self {
        let mut providers = HashMap::new();
        for p in providers_list {
            providers.insert(p.info().provider_type, p);
        }
        Self {
            providers,
            default_provider_type: Some(default_type),
            state: Arc::new(RwLock::new(state)),
            global_config,
            connection_error: None,
        }
    }

    /// Create a disconnected manager for testing
    #[cfg(any(test, feature = "test-support"))]
    pub fn disconnected_for_testing(
        global_config: GlobalConfig,
        state: StateStore,
        error: String,
    ) -> Self {
        Self {
            providers: HashMap::new(),
            default_provider_type: None,
            state: Arc::new(RwLock::new(state)),
            global_config,
            connection_error: Some(error),
        }
    }

    /// Create a disconnected manager (no provider available)
    pub fn disconnected(global_config: GlobalConfig, error: String) -> Result<Self> {
        let state = StateStore::load()?;

        Ok(Self {
            providers: HashMap::new(),
            default_provider_type: None,
            state: Arc::new(RwLock::new(state)),
            global_config,
            connection_error: Some(error),
        })
    }

    /// Check if connected to a provider
    pub fn is_connected(&self) -> bool {
        !self.providers.is_empty()
    }

    /// Get the connection error message (if disconnected)
    pub fn connection_error(&self) -> Option<&str> {
        self.connection_error.as_deref()
    }

    /// Connect to a provider (for reconnection)
    pub fn connect(&mut self, provider: Box<dyn ContainerProvider>) {
        let pt = provider.info().provider_type;
        self.providers.insert(pt, provider);
        self.default_provider_type = Some(pt);
        self.connection_error = None;
    }

    /// Get a provider for the given type
    fn require_provider_for(&self, pt: ProviderType) -> Result<&dyn ContainerProvider> {
        self.providers.get(&pt).map(|p| p.as_ref()).ok_or_else(|| {
            CoreError::NotConnected(format!("{} provider not available", pt))
        })
    }

    /// Get the provider matching a container's stored provider type
    fn require_container_provider(&self, cs: &ContainerState) -> Result<&dyn ContainerProvider> {
        self.require_provider_for(cs.provider)
    }

    /// Get the default provider, returning an error if not connected
    fn require_provider(&self) -> Result<&dyn ContainerProvider> {
        let pt = self.default_provider_type.ok_or_else(|| {
            CoreError::NotConnected(
                self.connection_error
                    .clone()
                    .unwrap_or_else(|| "No container provider available".to_string()),
            )
        })?;
        self.require_provider_for(pt)
    }

    /// Load a Container from config, using this manager's GlobalConfig
    /// instead of loading from disk (so test overrides are respected).
    fn load_container(&self, config_path: &Path) -> Result<Container> {
        let mut container = Container::from_config(config_path)?;
        container.global_config = self.global_config.clone();
        Ok(container)
    }

    /// Get the default provider type (None if disconnected)
    pub fn provider_type(&self) -> Option<ProviderType> {
        self.default_provider_type
    }

    /// Get a reference to the default container provider (for advanced operations like port detection)
    pub fn provider(&self) -> Option<&dyn ContainerProvider> {
        self.default_provider_type
            .and_then(|pt| self.providers.get(&pt))
            .map(|p| p.as_ref())
    }

    /// Get a reference to a provider for a specific type (for cross-provider operations)
    pub fn provider_for_type(&self, pt: ProviderType) -> Option<&dyn ContainerProvider> {
        self.providers.get(&pt).map(|p| p.as_ref())
    }

    /// Get runtime command args for a container's provider (for PTY shell, socat, etc.)
    /// Returns (program, prefix_args) so callers can build:
    /// `program [prefix_args...] exec [flags...] container_id [cmd...]`
    pub fn runtime_args_for(&self, cs: &ContainerState) -> Result<(String, Vec<String>)> {
        let provider = self.require_container_provider(cs)?;
        Ok(provider.runtime_args())
    }

    /// Get the global config
    pub fn global_config(&self) -> &GlobalConfig {
        &self.global_config
    }

    /// Set up credential forwarding for a container and return status.
    ///
    /// This is idempotent — safe to call before every shell/exec.
    /// Returns credential status for user-visible reporting.
    pub async fn setup_credentials_for_container(
        &self,
        id: &str,
    ) -> Result<crate::credentials::CredentialStatus> {
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        let provider = self.require_container_provider(&container_state)?;

        if container_state.status != DevcContainerStatus::Running {
            return Ok(crate::credentials::CredentialStatus::default());
        }

        let container_id = container_state
            .container_id
            .as_ref()
            .ok_or_else(|| CoreError::InvalidState("Container has no container ID".to_string()))?;
        let cid = ContainerId::new(container_id);

        let user = self.load_container(&container_state.config_path)
            .ok()
            .and_then(|c| c.devcontainer.effective_user().map(|s| s.to_string()));

        crate::credentials::setup_credentials(
            provider,
            &cid,
            &self.global_config,
            user.as_deref(),
            &container_state.workspace_path,
        )
        .await
    }

    /// Save state to disk without holding the write lock during I/O.
    ///
    /// Serializes under a read lock, then writes to disk after releasing it.
    /// Use this after modifying state in a write-lock scope to avoid
    /// holding the lock during disk I/O.
    pub(crate) async fn save_state(&self) -> Result<()> {
        let (content, path) = {
            let state = self.state.read().await;
            let content = state.serialize()?;
            let path = StateStore::state_path()?;
            (content, path)
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        crate::state::atomic_write(&path, content.as_bytes())?;
        Ok(())
    }

    /// List all managed containers
    pub async fn list(&self) -> Result<Vec<ContainerState>> {
        let state = self.state.read().await;
        Ok(state.list().into_iter().cloned().collect())
    }

    /// Get a container by name
    pub async fn get_by_name(&self, name: &str) -> Result<Option<ContainerState>> {
        let state = self.state.read().await;
        Ok(state.find_by_name(name).cloned())
    }

    /// Get a container by ID
    pub async fn get(&self, id: &str) -> Result<Option<ContainerState>> {
        let state = self.state.read().await;
        Ok(state.get(id).cloned())
    }

    /// Initialize a new container from a workspace
    pub async fn init(&self, workspace_path: &Path) -> Result<ContainerState> {
        let provider_type = self
            .provider_type()
            .ok_or_else(|| CoreError::NotConnected("Cannot init: no provider available".to_string()))?;

        let container = Container::from_workspace(workspace_path)?;

        let container_state = {
            let mut state = self.state.write().await;

            // Check if already exists (by config path for multi-config support)
            if let Some(existing) = state.find_by_config_path(&container.config_path) {
                return Err(CoreError::ContainerExists(existing.name.clone()));
            }

            let container_state = ContainerState::new(
                container.name.clone(),
                provider_type,
                container.config_path.clone(),
                container.workspace_path.clone(),
            );

            state.add(container_state.clone());
            container_state
        };
        self.save_state().await?;

        Ok(container_state)
    }

    /// Create a container from a built image
    pub async fn create(&self, id: &str) -> Result<ContainerId> {
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        let provider = self.require_container_provider(&container_state)?;

        let image_id = container_state.image_id.as_ref().ok_or_else(|| {
            CoreError::InvalidState("Container image not built yet".to_string())
        })?;

        let container = self.load_container(&container_state.config_path)?;

        // Deserialize feature properties from build metadata (if any)
        let feature_props = container_state
            .metadata
            .get("feature_properties")
            .and_then(|json| serde_json::from_str::<features::MergedFeatureProperties>(json).ok());

        let mut create_config = container.create_config_with_features(
            image_id,
            feature_props.as_ref(),
        );

        // Add tmpfs mount for credential cache if credential forwarding is enabled
        if self.global_config.credentials.docker || self.global_config.credentials.git {
            create_config.mounts.push(devc_provider::MountConfig {
                mount_type: devc_provider::MountType::Tmpfs,
                source: String::new(),
                target: crate::credentials::inject::CREDS_TMPFS_PATH.to_string(),
                read_only: false,
            });
        }

        // Clean up any orphaned container with the same name before creating
        // This handles cases where state has container_id=null but a container exists
        let container_name = container.container_name();
        provider.remove_by_name(&container_name).await?;

        let container_id = provider.create(&create_config).await?;

        // Update state with container ID
        {
            let mut state = self.state.write().await;
            if let Some(cs) = state.get_mut(id) {
                cs.container_id = Some(container_id.0.clone());
                cs.status = DevcContainerStatus::Created;
            }
        }
        self.save_state().await?;

        Ok(container_id)
    }

    /// Start a container
    pub async fn start(&self, id: &str) -> Result<()> {
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        let provider = self.require_container_provider(&container_state)?;

        // Allow idempotent call when already running — skips provider.start()
        // but still runs post-start phase (SSH daemon, postStartCommand)
        if container_state.status != DevcContainerStatus::Running && !container_state.can_start() {
            return Err(CoreError::InvalidState(format!(
                "Container cannot be started in {} state",
                container_state.status
            )));
        }

        // Handle compose start: bring up all services
        let is_compose = container_state.compose_project.is_some()
            || self.load_container(&container_state.config_path)
                .map(|c| c.is_compose())
                .unwrap_or(false);
        if is_compose {
            let container = self.load_container(&container_state.config_path)?;
            if let Some(compose_files) = container.compose_files() {
                let owned = compose_file_strs(&compose_files);
                let compose_file_refs: Vec<&str> =
                    owned.iter().map(|s| s.as_str()).collect();
                let project_name = container.compose_project_name();

                provider
                    .compose_up(
                        &compose_file_refs,
                        &project_name,
                        &container.workspace_path,
                        None,
                    )
                    .await?;

                // Re-discover the primary service container ID after compose_up
                let services = provider
                    .compose_ps(&compose_file_refs, &project_name, &container.workspace_path)
                    .await?;
                let primary_service = container.compose_service().ok_or_else(|| {
                    CoreError::InvalidState(
                        "No service specified for compose project".to_string(),
                    )
                })?;
                let svc = services
                    .iter()
                    .find(|s| s.service_name == primary_service)
                    .ok_or_else(|| {
                        CoreError::InvalidState(format!(
                            "Service '{}' not found in compose project",
                            primary_service
                        ))
                    })?;
                {
                    let mut state = self.state.write().await;
                    if let Some(cs) = state.get_mut(id) {
                        cs.container_id = Some(svc.container_id.0.clone());
                        cs.compose_project = Some(project_name);
                        cs.compose_service = Some(primary_service.to_string());
                    }
                }

                self.set_status(id, DevcContainerStatus::Running).await?;

                // Ensure SSH daemon is running if SSH was set up
                if container_state.metadata.get("ssh_available").map(|v| v == "true").unwrap_or(false) {
                    self.ensure_ssh_daemon_running(provider, &svc.container_id).await?;
                }

                // Run post-start commands (feature commands first, then devcontainer.json)
                let feature_props = get_feature_properties(&container_state);
                let merged_env = merge_remote_env(
                    container.devcontainer.remote_env.as_ref(),
                    &feature_props.remote_env,
                );
                if !feature_props.post_start_commands.is_empty() {
                    run_feature_lifecycle_commands(
                        provider,
                        &svc.container_id,
                        &feature_props.post_start_commands,
                        container.devcontainer.effective_user(),
                        container.devcontainer.workspace_folder.as_deref(),
                        merged_env.as_ref(),
                    )
                    .await?;
                }
                if let Some(ref cmd) = container.devcontainer.post_start_command {
                    run_lifecycle_command_with_env(
                        provider,
                        &svc.container_id,
                        cmd,
                        container.devcontainer.effective_user(),
                        container.devcontainer.workspace_folder.as_deref(),
                        merged_env.as_ref(),
                    )
                    .await?;
                }

                return Ok(());
            }
        }

        let container_id = container_state.container_id.as_ref().ok_or_else(|| {
            CoreError::InvalidState("Container not created yet".to_string())
        })?;

        // Only call provider.start() if the container is not already running
        let details = provider.inspect(&ContainerId::new(container_id)).await?;
        if details.status != ContainerStatus::Running {
            provider.start(&ContainerId::new(container_id)).await?;
        }

        // Update status
        self.set_status(id, DevcContainerStatus::Running).await?;

        // Ensure SSH daemon is running if SSH was set up for this container
        if container_state.metadata.get("ssh_available").map(|v| v == "true").unwrap_or(false) {
            self.ensure_ssh_daemon_running(provider, &ContainerId::new(container_id)).await?;
        }

        // Run post-start commands (feature commands first, then devcontainer.json)
        let container = self.load_container(&container_state.config_path)?;
        let feature_props = get_feature_properties(&container_state);
        let merged_env = merge_remote_env(
            container.devcontainer.remote_env.as_ref(),
            &feature_props.remote_env,
        );
        let cid = ContainerId::new(container_id);
        if !feature_props.post_start_commands.is_empty() {
            run_feature_lifecycle_commands(
                provider,
                &cid,
                &feature_props.post_start_commands,
                container.devcontainer.effective_user(),
                container.devcontainer.workspace_folder.as_deref(),
                merged_env.as_ref(),
            )
            .await?;
        }
        if let Some(ref cmd) = container.devcontainer.post_start_command {
            run_lifecycle_command_with_env(
                provider,
                &cid,
                cmd,
                container.devcontainer.effective_user(),
                container.devcontainer.workspace_folder.as_deref(),
                merged_env.as_ref(),
            )
            .await?;
        }

        Ok(())
    }

    /// Ensure the SSH daemon (dropbear) is running in the container
    async fn ensure_ssh_daemon_running(&self, provider: &dyn ContainerProvider, container_id: &ContainerId) -> Result<()> {
        let script = r#"
if ! pgrep -x dropbear >/dev/null 2>&1; then
    /usr/sbin/dropbear -s -r /etc/dropbear/dropbear_ed25519_host_key -p 127.0.0.1:2222 2>/dev/null
fi
"#;
        let config = devc_provider::ExecConfig {
            cmd: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
            env: std::collections::HashMap::new(),
            working_dir: None,
            user: Some("root".to_string()),
            tty: false,
            stdin: false,
            privileged: false,
        };

        match provider.exec(container_id, &config).await {
            Ok(_) => {
                tracing::debug!("SSH daemon check/start completed");
                Ok(())
            }
            Err(e) => {
                tracing::warn!("Failed to ensure SSH daemon is running: {}", e);
                // Don't fail the start if SSH daemon can't be started
                Ok(())
            }
        }
    }

    /// Stop a container (or all compose services for a compose project)
    pub async fn stop(&self, id: &str) -> Result<()> {
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        let provider = self.require_container_provider(&container_state)?;

        if !container_state.can_stop() {
            return Err(CoreError::InvalidState(format!(
                "Container cannot be stopped in {} state",
                container_state.status
            )));
        }

        // Handle compose stop: bring down all services
        if let Some(ref compose_project) = container_state.compose_project {
            let container = self.load_container(&container_state.config_path)?;
            if let Some(compose_files) = container.compose_files() {
                let owned = compose_file_strs(&compose_files);
                let compose_file_refs: Vec<&str> =
                    owned.iter().map(|s| s.as_str()).collect();

                provider
                    .compose_down(
                        &compose_file_refs,
                        compose_project,
                        &container.workspace_path,
                    )
                    .await?;

                // Clear container_id since containers are destroyed by compose_down.
                // Keep compose_project and compose_service so start() can detect
                // this is a compose project and call compose_up to recreate services.
                {
                    let mut state = self.state.write().await;
                    if let Some(cs) = state.get_mut(id) {
                        cs.container_id = None;
                    }
                }

                self.set_status(id, DevcContainerStatus::Stopped).await?;
                return Ok(());
            }
        }

        let container_id = container_state.container_id.as_ref().ok_or_else(|| {
            CoreError::InvalidState("Container not created".to_string())
        })?;

        provider
            .stop(&ContainerId::new(container_id), Some(10))
            .await?;

        self.set_status(id, DevcContainerStatus::Stopped).await?;

        Ok(())
    }

    /// Remove a container completely (removes from state store too)
    pub async fn remove(&self, id: &str, force: bool) -> Result<()> {
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        if !force && !container_state.can_remove() {
            return Err(CoreError::InvalidState(format!(
                "Container cannot be removed in {} state (use force to override)",
                container_state.status
            )));
        }

        // Only destroy the runtime container if devc created it
        if container_state.source == DevcontainerSource::Devc {
            if let Some(ref container_id) = container_state.container_id {
                if let Some(provider) = self.providers.get(&container_state.provider) {
                    if let Err(e) = provider
                        .remove(&ContainerId::new(container_id), force)
                        .await
                    {
                        tracing::warn!("Failed to remove container {}: {}", container_id, e);
                    }
                }
            }
        } else {
            tracing::info!(
                "Skipping runtime destroy for adopted container '{}' (source: {:?})",
                container_state.name,
                container_state.source,
            );
        }

        // Remove from state
        {
            let mut state = self.state.write().await;
            state.remove(id);
        }
        self.save_state().await?;

        Ok(())
    }

    /// Stop and remove the runtime container, but keep the state so it can be recreated with `up`
    pub async fn down(&self, id: &str) -> Result<()> {
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        // For adopted containers, skip runtime teardown — only clean up tracking state
        if container_state.source != DevcontainerSource::Devc {
            tracing::info!(
                "Skipping runtime stop/remove for adopted container '{}' (source: {:?})",
                container_state.name,
                container_state.source,
            );
        } else {
            let provider = self.require_container_provider(&container_state)?;

            // Handle compose teardown
            if let Some(ref compose_project) = container_state.compose_project {
                let container = self.load_container(&container_state.config_path)?;
                if let Some(compose_files) = container.compose_files() {
                    let owned = compose_file_strs(&compose_files);
                    let compose_file_refs: Vec<&str> =
                        owned.iter().map(|s| s.as_str()).collect();

                    if let Err(e) = provider
                        .compose_down(
                            &compose_file_refs,
                            compose_project,
                            &container.workspace_path,
                        )
                        .await
                    {
                        tracing::warn!("Failed to run compose down: {}", e);
                    }
                }
            } else {
                // Standard single-container teardown
                // Stop if running
                if container_state.status == DevcContainerStatus::Running {
                    if let Some(ref container_id) = container_state.container_id {
                        if let Err(e) = provider
                            .stop(&ContainerId::new(container_id), Some(10))
                            .await
                        {
                            tracing::warn!("Failed to stop container {}: {}", container_id, e);
                        }
                    }
                }

                // Remove the runtime container if it exists
                if let Some(ref container_id) = container_state.container_id {
                    if let Err(e) = provider
                        .remove(&ContainerId::new(container_id), true)
                        .await
                    {
                        tracing::warn!("Failed to remove container {}: {}", container_id, e);
                    }
                }
            }
        }

        // Update state: keep image but clear container_id, reset status to Built (or Configured if no image)
        {
            let mut state = self.state.write().await;
            if let Some(cs) = state.get_mut(id) {
                cs.container_id = None;
                cs.compose_project = None;
                cs.compose_service = None;
                cs.status = if cs.image_id.is_some() {
                    DevcContainerStatus::Built
                } else {
                    DevcContainerStatus::Configured
                };
                // Clear SSH metadata since we'll need to set it up again
                cs.metadata.remove("ssh_available");
            }
        }
        self.save_state().await?;

        Ok(())
    }

    /// Build, create, and start a container (full lifecycle)
    pub async fn up(&self, id: &str) -> Result<()> {
        self.up_with_progress(id, None, None).await
    }

    /// Build, create, and start a container with progress updates
    pub async fn up_with_progress(
        &self,
        id: &str,
        progress: Option<&mpsc::UnboundedSender<String>>,
        output: Option<&mpsc::UnboundedSender<String>>,
    ) -> Result<()> {
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        let provider = self.require_container_provider(&container_state)?;

        let container = self.load_container(&container_state.config_path)?;
        if let Some(ref wait_for) = container.devcontainer.wait_for {
            tracing::info!("waitFor is set to '{}' (async lifecycle deferral not yet implemented)", wait_for);
        }

        // Handle Docker Compose projects
        if container.is_compose() {
            return self
                .up_compose(id, &container, &container_state, provider, progress, output)
                .await;
        }

        // Build if needed
        if container_state.image_id.is_none() {
            // initializeCommand runs on host before build (per spec)
            if let Some(ref cmd) = container.devcontainer.initialize_command {
                send_progress(progress, "Running initializeCommand on host...");
                crate::run_host_command(cmd, &container.workspace_path, output).await?;
            }
            send_progress(progress, "Building image...");
            self.build(id).await?;
        }

        // Create if needed
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        if container_state.container_id.is_none() {
            send_progress(progress, "Creating container...");
            self.create(id).await?;
        }

        // Get the container ID (re-read state after create may have modified it)
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };
        let container_id = ContainerId::new(
            container_state
                .container_id
                .as_ref()
                .ok_or_else(|| {
                    CoreError::InvalidState(format!(
                        "Container '{}' has no runtime container ID after create",
                        id
                    ))
                })?,
        );

        // Run first-create lifecycle if this is a newly created container
        if container_state.status == DevcContainerStatus::Created {
            self.run_first_create_lifecycle(
                id, &container, provider, &container_id, progress,
            ).await?;
        }

        // Start container (idempotent) and run post-start phase
        send_progress(progress, "Starting container...");
        self.start(id).await?;

        // Set up credential forwarding so it's ready for shell access
        if let Err(e) = crate::credentials::setup_credentials(
            provider,
            &container_id,
            &self.global_config,
            container.devcontainer.effective_user(),
            &container_state.workspace_path,
        )
        .await
        {
            tracing::warn!("Credential forwarding setup failed (non-fatal): {}", e);
        }

        Ok(())
    }

    /// Sync container status with actual provider status
    ///
    /// Creates a provider matching the container's own provider type to inspect it,
    /// so cross-provider containers (e.g. adopted from a different runtime) are
    /// inspected correctly. Returns current status if the provider can't be created.
    pub async fn sync_status(&self, id: &str) -> Result<DevcContainerStatus> {
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        // Look up the provider matching the container's own type.
        // Fall back to current status if the provider isn't available.
        let provider = match self.require_container_provider(&container_state) {
            Ok(p) => p,
            Err(_) => return Ok(container_state.status),
        };

        let new_status = if let Some(ref container_id) = container_state.container_id {
            match provider.inspect(&ContainerId::new(container_id)).await {
                Ok(details) => match details.status {
                    ContainerStatus::Running => DevcContainerStatus::Running,
                    ContainerStatus::Exited | ContainerStatus::Dead => DevcContainerStatus::Stopped,
                    ContainerStatus::Created | ContainerStatus::Paused => {
                        DevcContainerStatus::Created
                    }
                    _ => container_state.status,
                },
                Err(_) => {
                    // Container doesn't exist anymore
                    if container_state.image_id.is_some() {
                        DevcContainerStatus::Built
                    } else {
                        DevcContainerStatus::Configured
                    }
                }
            }
        } else {
            container_state.status
        };

        if new_status != container_state.status {
            self.set_status(id, new_status).await?;
        }

        Ok(new_status)
    }

    /// Get container logs
    ///
    /// Returns logs as a vector of lines. If tail is specified, only returns
    /// that many lines from the end.
    pub async fn logs(&self, id: &str, tail: Option<u64>) -> Result<Vec<String>> {
        use tokio::io::AsyncBufReadExt;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        let provider = self.require_container_provider(&container_state)?;

        let container_id = container_state
            .container_id
            .as_ref()
            .ok_or_else(|| CoreError::InvalidState("Container has no container ID".to_string()))?;

        let config = LogConfig {
            follow: false,
            stdout: true,
            stderr: true,
            tail,
            timestamps: false,
            since: None,
            until: None,
        };

        let log_stream = provider
            .logs(&ContainerId::new(container_id), &config)
            .await?;

        // Read all lines from the stream
        let reader = tokio::io::BufReader::new(log_stream.stream);
        let mut lines = reader.lines();
        let mut result = Vec::new();

        while let Some(line) = lines.next_line().await? {
            result.push(line);
        }

        Ok(result)
    }

    /// Helper to set container status
    async fn set_status(&self, id: &str, status: DevcContainerStatus) -> Result<()> {
        {
            let mut state = self.state.write().await;
            if let Some(cs) = state.get_mut(id) {
                cs.status = status;
            }
        }
        self.save_state().await?;
        Ok(())
    }

    /// Load the devcontainer config for a given container state.
    ///
    /// This is useful for reading port forwarding configuration, compose files,
    /// and other settings from the devcontainer.json.
    pub fn get_devcontainer_config(
        &self,
        state: &ContainerState,
    ) -> Result<devc_config::DevContainerConfig> {
        let container = self.load_container(&state.config_path)?;
        Ok(container.devcontainer)
    }

}

/// Convert a slice of PathBuf compose files to owned Strings and borrowed &str refs.
///
/// Returns (owned, refs) where `refs` borrows from `owned`.
/// Caller must keep `owned` alive while using `refs`.
pub(crate) fn compose_file_strs(files: &[std::path::PathBuf]) -> Vec<String> {
    files.iter().map(|f| f.to_string_lossy().to_string()).collect()
}

/// Extract merged feature properties from container state metadata.
pub(crate) fn get_feature_properties(state: &ContainerState) -> features::MergedFeatureProperties {
    state
        .metadata
        .get("feature_properties")
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
}

/// Merge feature remoteEnv with devcontainer.json remoteEnv.
/// Feature env provides a base; devcontainer.json wins on conflict.
pub(crate) fn merge_remote_env(
    devcontainer_env: Option<&HashMap<String, String>>,
    feature_env: &HashMap<String, String>,
) -> Option<HashMap<String, String>> {
    if feature_env.is_empty() && devcontainer_env.is_none() {
        return None;
    }
    let mut merged = feature_env.clone();
    if let Some(dc_env) = devcontainer_env {
        merged.extend(dc_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    Some(merged)
}

pub(crate) fn send_progress(progress: Option<&mpsc::UnboundedSender<String>>, msg: &str) {
    if let Some(tx) = progress {
        let _ = tx.send(msg.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use devc_provider::{ComposeServiceInfo, ContainerStatus, ProviderError, ProviderType};

    /// Create a test workspace with a devcontainer.json that uses an image
    fn create_test_workspace() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let devcontainer_dir = tmp.path().join(".devcontainer");
        std::fs::create_dir_all(&devcontainer_dir).unwrap();
        std::fs::write(
            devcontainer_dir.join("devcontainer.json"),
            r#"{"image": "ubuntu:22.04"}"#,
        )
        .unwrap();
        tmp
    }

    /// Create a test manager with MockProvider, returning both manager and mock calls tracker
    fn test_manager(mock: MockProvider) -> ContainerManager {
        let state = StateStore::new();
        ContainerManager::new_for_testing(
            Box::new(mock),
            GlobalConfig::default(),
            state,
        )
    }

    /// Create a test manager with a pre-existing container state
    fn test_manager_with_state(mock: MockProvider, state: StateStore) -> ContainerManager {
        ContainerManager::new_for_testing(
            Box::new(mock),
            GlobalConfig::default(),
            state,
        )
    }

    /// Helper: create a ContainerState for use in StateStore
    fn make_container_state(
        workspace: &std::path::Path,
        status: DevcContainerStatus,
        image_id: Option<&str>,
        container_id: Option<&str>,
    ) -> ContainerState {
        let config_path = workspace
            .join(".devcontainer/devcontainer.json");
        let mut cs = ContainerState::new(
            "test".to_string(),
            ProviderType::Docker,
            config_path,
            workspace.to_path_buf(),
        );
        cs.status = status;
        cs.image_id = image_id.map(|s| s.to_string());
        cs.container_id = container_id.map(|s| s.to_string());
        cs
    }

    // ==================== Constructor / Connectivity ====================

    #[tokio::test]
    async fn test_disconnected_constructor() {
        let state = StateStore::new();
        let mgr = ContainerManager::disconnected_for_testing(
            GlobalConfig::default(),
            state,
            "docker not found".to_string(),
        );
        assert!(!mgr.is_connected());
        assert_eq!(mgr.connection_error(), Some("docker not found"));
    }

    #[tokio::test]
    async fn test_require_provider_when_disconnected() {
        let workspace = create_test_workspace();
        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("img123"),
            Some("ctr123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = ContainerManager::disconnected_for_testing(
            GlobalConfig::default(),
            state,
            "no runtime".to_string(),
        );
        // Operation on an existing container should fail with provider error
        let result = mgr.stop(&id).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("provider not available"), "Expected 'provider not available' but got: {}", err_msg);
    }

    #[tokio::test]
    async fn test_connect_reconnects() {
        let state = StateStore::new();
        let mut mgr = ContainerManager::disconnected_for_testing(
            GlobalConfig::default(),
            state,
            "disconnected".to_string(),
        );
        assert!(!mgr.is_connected());

        let mock = MockProvider::new(ProviderType::Docker);
        mgr.connect(Box::new(mock));

        assert!(mgr.is_connected());
        assert!(mgr.connection_error().is_none());
    }

    #[tokio::test]
    async fn test_provider_type_connected() {
        let mock = MockProvider::new(ProviderType::Podman);
        let mgr = test_manager(mock);
        assert_eq!(mgr.provider_type(), Some(ProviderType::Podman));
    }

    #[tokio::test]
    async fn test_provider_type_disconnected() {
        let state = StateStore::new();
        let mgr = ContainerManager::disconnected_for_testing(
            GlobalConfig::default(),
            state,
            "err".to_string(),
        );
        assert_eq!(mgr.provider_type(), None);
    }

    // ==================== Init ====================

    #[tokio::test]
    async fn test_init_creates_state() {
        let mock = MockProvider::new(ProviderType::Docker);
        let mgr = test_manager(mock);
        let workspace = create_test_workspace();

        let cs = mgr.init(workspace.path()).await.unwrap();
        assert_eq!(cs.status, DevcContainerStatus::Configured);
        assert_eq!(cs.provider, ProviderType::Docker);
        assert!(cs.image_id.is_none());
        assert!(cs.container_id.is_none());

        // Verify it's retrievable
        let found = mgr.get(&cs.id).await.unwrap();
        assert!(found.is_some());
    }

    #[tokio::test]
    async fn test_init_duplicate_fails() {
        let mock = MockProvider::new(ProviderType::Docker);
        let mgr = test_manager(mock);
        let workspace = create_test_workspace();

        mgr.init(workspace.path()).await.unwrap();
        let result = mgr.init(workspace.path()).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("already exists"));
    }

    #[tokio::test]
    async fn test_init_disconnected_fails() {
        let state = StateStore::new();
        let mgr = ContainerManager::disconnected_for_testing(
            GlobalConfig::default(),
            state,
            "no provider".to_string(),
        );
        let workspace = create_test_workspace();

        let result = mgr.init(workspace.path()).await;
        assert!(result.is_err());
    }

    // ==================== Build ====================

    #[tokio::test]
    async fn test_build_pulls_image() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Configured,
            None,
            None,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let image_id = mgr.build(&id).await.unwrap();
        assert!(!image_id.is_empty());

        // ssh_enabled is false by default, so image-based should call pull
        let recorded = calls.lock().unwrap();
        assert!(recorded.iter().any(|c| matches!(c, MockCall::Pull { .. })));
    }

    #[tokio::test]
    async fn test_build_sets_failed_on_error() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        *mock.pull_result.lock().unwrap() =
            Err(ProviderError::RuntimeError("pull failed".into()));

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Configured,
            None,
            None,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let result = mgr.build(&id).await;
        assert!(result.is_err());

        // Status should be Failed
        let cs = mgr.get(&id).await.unwrap().unwrap();
        assert_eq!(cs.status, DevcContainerStatus::Failed);
    }

    #[tokio::test]
    async fn test_build_compose_skips_build() {
        let workspace = create_test_workspace();
        // Write a compose-based devcontainer.json
        std::fs::write(
            workspace.path().join(".devcontainer/devcontainer.json"),
            r#"{"dockerComposeFile": "docker-compose.yml", "service": "app"}"#,
        )
        .unwrap();

        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();
        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Configured,
            None,
            None,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let result = mgr.build(&id).await;
        assert!(result.is_ok(), "Compose build should succeed (skip)");
        assert_eq!(result.unwrap(), "compose");

        // Status should be Built
        let cs = mgr.get(&id).await.unwrap().unwrap();
        assert_eq!(cs.status, DevcContainerStatus::Built);
        assert_eq!(cs.image_id, Some("compose".to_string()));

        // Should NOT have called any provider build/pull
        let recorded = calls.lock().unwrap();
        assert!(!recorded.iter().any(|c| matches!(c, MockCall::Build { .. })));
        assert!(!recorded.iter().any(|c| matches!(c, MockCall::Pull { .. })));
    }

    #[tokio::test]
    async fn test_build_no_source_fails() {
        let workspace = create_test_workspace();
        // Write a devcontainer.json with no image source
        std::fs::write(
            workspace.path().join(".devcontainer/devcontainer.json"),
            r#"{"name": "empty"}"#,
        )
        .unwrap();

        let mock = MockProvider::new(ProviderType::Docker);
        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Configured,
            None,
            None,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let result = mgr.build(&id).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("No image source"));
    }

    #[tokio::test]
    async fn test_build_calls_provider_build() {
        let workspace = create_test_workspace();
        // Dockerfile-based config
        std::fs::write(
            workspace.path().join(".devcontainer/devcontainer.json"),
            r#"{"build": {"dockerfile": "Dockerfile"}}"#,
        )
        .unwrap();
        std::fs::write(
            workspace.path().join(".devcontainer/Dockerfile"),
            "FROM ubuntu:22.04\n",
        )
        .unwrap();

        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Configured,
            None,
            None,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.build(&id).await.unwrap();

        // Should call build, not pull
        let recorded = calls.lock().unwrap();
        assert!(recorded.iter().any(|c| matches!(c, MockCall::Build { .. })));
        assert!(!recorded.iter().any(|c| matches!(c, MockCall::Pull { .. })));
    }

    // ==================== Create ====================

    #[tokio::test]
    async fn test_create_requires_image_id() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Configured,
            None, // no image_id
            None,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let result = mgr.create(&id).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("not built yet"));
    }

    #[tokio::test]
    async fn test_create_sets_container_id() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Built,
            Some("sha256:image123"),
            None,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let container_id = mgr.create(&id).await.unwrap();
        assert_eq!(container_id.0, "mock_container_id");

        let cs = mgr.get(&id).await.unwrap().unwrap();
        assert_eq!(cs.container_id, Some("mock_container_id".to_string()));
        assert_eq!(cs.status, DevcContainerStatus::Created);
    }

    #[tokio::test]
    async fn test_create_cleans_orphan() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Built,
            Some("sha256:image123"),
            None,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.create(&id).await.unwrap();

        // Should have called remove_by_name before create
        let recorded = calls.lock().unwrap();
        assert!(recorded
            .iter()
            .any(|c| matches!(c, MockCall::RemoveByName { .. })));
    }

    // ==================== Start / Stop ====================

    #[tokio::test]
    async fn test_start_sets_running() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Created,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.start(&id).await.unwrap();

        let cs = mgr.get(&id).await.unwrap().unwrap();
        assert_eq!(cs.status, DevcContainerStatus::Running);
    }

    #[tokio::test]
    async fn test_start_idempotent_when_running() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running, // already running
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        // start() should succeed when already running (idempotent)
        mgr.start(&id).await.unwrap();

        // Should NOT have called provider.start() since container is already running
        let recorded = calls.lock().unwrap();
        assert!(!recorded.iter().any(|c| matches!(c, MockCall::Start { .. })));
    }

    #[tokio::test]
    async fn test_start_invalid_state_fails() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Building, // can't start from Building
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let result = mgr.start(&id).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("cannot be started"));
    }

    #[tokio::test]
    async fn test_start_runs_post_start() {
        let workspace = create_test_workspace();
        // Add a postStartCommand
        std::fs::write(
            workspace.path().join(".devcontainer/devcontainer.json"),
            r#"{"image": "ubuntu:22.04", "postStartCommand": "echo hello"}"#,
        )
        .unwrap();

        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Stopped,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.start(&id).await.unwrap();

        // Should have called exec for postStartCommand
        let recorded = calls.lock().unwrap();
        assert!(recorded.iter().any(|c| matches!(c, MockCall::Exec { .. })));
    }

    #[tokio::test]
    async fn test_stop_sets_stopped() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.stop(&id).await.unwrap();

        let cs = mgr.get(&id).await.unwrap().unwrap();
        assert_eq!(cs.status, DevcContainerStatus::Stopped);
    }

    #[tokio::test]
    async fn test_stop_invalid_state_fails() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Stopped, // already stopped
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let result = mgr.stop(&id).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("cannot be stopped"));
    }

    // ==================== Remove ====================

    #[tokio::test]
    async fn test_remove_from_state() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Stopped,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.remove(&id, false).await.unwrap();

        let cs = mgr.get(&id).await.unwrap();
        assert!(cs.is_none(), "Container should be removed from state");
    }

    #[tokio::test]
    async fn test_remove_force_running() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        // Force remove should work even on running containers
        mgr.remove(&id, true).await.unwrap();

        let cs = mgr.get(&id).await.unwrap();
        assert!(cs.is_none());
    }

    #[tokio::test]
    async fn test_remove_no_force_running_fails() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let result = mgr.remove(&id, false).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("cannot be removed"));
    }

    #[tokio::test]
    async fn test_remove_disconnected_removes_state() {
        let workspace = create_test_workspace();
        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Stopped,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = ContainerManager::disconnected_for_testing(
            GlobalConfig::default(),
            state,
            "no provider".to_string(),
        );

        // Should still remove from state even without provider
        mgr.remove(&id, false).await.unwrap();
        let cs = mgr.get(&id).await.unwrap();
        assert!(cs.is_none());
    }

    // ==================== Down ====================

    #[tokio::test]
    async fn test_down_stops_and_removes() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.down(&id).await.unwrap();

        // Should have called stop + remove on provider
        let recorded = calls.lock().unwrap();
        assert!(recorded.iter().any(|c| matches!(c, MockCall::Stop { .. })));
        assert!(recorded
            .iter()
            .any(|c| matches!(c, MockCall::Remove { force: true, .. })));

        // State should still exist but container_id cleared
        let cs = mgr.get(&id).await.unwrap().unwrap();
        assert!(cs.container_id.is_none());
        assert_eq!(cs.status, DevcContainerStatus::Built);
    }

    #[tokio::test]
    async fn test_down_clears_ssh_metadata() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let mut cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        cs.metadata
            .insert("ssh_available".to_string(), "true".to_string());
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.down(&id).await.unwrap();

        let cs = mgr.get(&id).await.unwrap().unwrap();
        assert!(!cs.metadata.contains_key("ssh_available"));
    }

    #[tokio::test]
    async fn test_down_sets_built_status() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.down(&id).await.unwrap();

        let cs = mgr.get(&id).await.unwrap().unwrap();
        assert_eq!(cs.status, DevcContainerStatus::Built);
    }

    #[tokio::test]
    async fn test_down_no_image_sets_configured() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            None, // no image
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.down(&id).await.unwrap();

        let cs = mgr.get(&id).await.unwrap().unwrap();
        assert_eq!(cs.status, DevcContainerStatus::Configured);
    }

    // ==================== Rebuild ====================

    #[tokio::test]
    async fn test_rebuild_disconnected_fails() {
        let workspace = create_test_workspace();
        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = ContainerManager::disconnected_for_testing(
            GlobalConfig::default(),
            state,
            "no provider".to_string(),
        );

        let result = mgr.rebuild(&id, false).await;
        assert!(result.is_err());
    }

    // ==================== Sync Status ====================

    #[tokio::test]
    async fn test_sync_running_stays_running() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        // Default inspect returns Running

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let status = mgr.sync_status(&id).await.unwrap();
        assert_eq!(status, DevcContainerStatus::Running);
    }

    #[tokio::test]
    async fn test_sync_running_to_stopped() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        *mock.inspect_result.lock().unwrap() = Ok(mock_container_details(
            "container123",
            ContainerStatus::Exited,
        ));

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let status = mgr.sync_status(&id).await.unwrap();
        assert_eq!(status, DevcContainerStatus::Stopped);
    }

    #[tokio::test]
    async fn test_sync_container_disappeared() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        *mock.inspect_result.lock().unwrap() =
            Err(ProviderError::ContainerNotFound("gone".into()));

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let status = mgr.sync_status(&id).await.unwrap();
        // Container had an image, so should be Built
        assert_eq!(status, DevcContainerStatus::Built);
    }

    #[tokio::test]
    async fn test_sync_no_container_id_returns_current() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Built,
            Some("sha256:img"),
            None, // no container_id → returns current status without inspecting
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let status = mgr.sync_status(&id).await.unwrap();
        assert_eq!(status, DevcContainerStatus::Built);
    }

    // ==================== List / Get ====================

    #[tokio::test]
    async fn test_list_empty() {
        let mock = MockProvider::new(ProviderType::Docker);
        let mgr = test_manager(mock);
        let list = mgr.list().await.unwrap();
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn test_list_returns_all() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let cs1 = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("c1"),
        );
        state.add(cs1);

        let workspace2 = create_test_workspace();
        let cs2 = make_container_state(
            workspace2.path(),
            DevcContainerStatus::Stopped,
            Some("sha256:img2"),
            Some("c2"),
        );
        state.add(cs2);

        let mgr = test_manager_with_state(mock, state);
        let list = mgr.list().await.unwrap();
        assert_eq!(list.len(), 2);
    }

    #[tokio::test]
    async fn test_get_by_name() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Configured,
            None,
            None,
        );
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let found = mgr.get_by_name("test").await.unwrap();
        assert!(found.is_some());

        let not_found = mgr.get_by_name("nonexistent").await.unwrap();
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_get_nonexistent() {
        let mock = MockProvider::new(ProviderType::Docker);
        let mgr = test_manager(mock);
        let result = mgr.get("nonexistent-id").await.unwrap();
        assert!(result.is_none());
    }

    // ==================== Compose ====================

    #[tokio::test]
    async fn test_up_compose_calls_compose_up() {
        let workspace = create_test_workspace();
        // Write a compose-based devcontainer.json
        std::fs::write(
            workspace.path().join(".devcontainer/devcontainer.json"),
            r#"{"dockerComposeFile": "docker-compose.yml", "service": "app", "workspaceFolder": "/workspace"}"#,
        )
        .unwrap();
        // Create a dummy compose file (content doesn't matter for mock)
        std::fs::write(
            workspace.path().join(".devcontainer/docker-compose.yml"),
            "version: '3'\nservices:\n  app:\n    image: ubuntu:22.04\n",
        )
        .unwrap();

        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();
        // compose_ps returns a service entry
        *mock.compose_ps_result.lock().unwrap() = Ok(vec![ComposeServiceInfo {
            service_name: "app".to_string(),
            container_id: ContainerId::new("compose_container_123"),
            status: ContainerStatus::Running,
        }]);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Configured,
            None,
            None,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.up(&id).await.unwrap();

        // Verify compose_up was called
        let recorded = calls.lock().unwrap();
        assert!(recorded.iter().any(|c| matches!(c, MockCall::ComposeUp { .. })));
        assert!(recorded.iter().any(|c| matches!(c, MockCall::ComposePs { .. })));

        // Verify state was updated with compose metadata
        let cs = mgr.get(&id).await.unwrap().unwrap();
        assert_eq!(cs.container_id, Some("compose_container_123".to_string()));
        assert!(cs.compose_project.as_ref().unwrap().starts_with("devc-"));
        assert_eq!(cs.compose_service, Some("app".to_string()));
        assert_eq!(cs.status, DevcContainerStatus::Running);
    }

    #[tokio::test]
    async fn test_down_compose_calls_compose_down() {
        let workspace = create_test_workspace();
        // Write a compose-based devcontainer.json
        std::fs::write(
            workspace.path().join(".devcontainer/devcontainer.json"),
            r#"{"dockerComposeFile": "docker-compose.yml", "service": "app"}"#,
        )
        .unwrap();
        std::fs::write(
            workspace.path().join(".devcontainer/docker-compose.yml"),
            "version: '3'\nservices:\n  app:\n    image: ubuntu:22.04\n",
        )
        .unwrap();

        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let mut cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("compose"),
            Some("compose_container_123"),
        );
        cs.compose_project = Some("devc-test".to_string());
        cs.compose_service = Some("app".to_string());
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.down(&id).await.unwrap();

        // Verify compose_down was called
        let recorded = calls.lock().unwrap();
        assert!(recorded.iter().any(|c| matches!(c, MockCall::ComposeDown { .. })));
        // Should NOT call individual stop/remove
        assert!(!recorded.iter().any(|c| matches!(c, MockCall::Stop { .. })));
        assert!(!recorded.iter().any(|c| matches!(c, MockCall::Remove { .. })));

        // State should be cleaned up
        let cs = mgr.get(&id).await.unwrap().unwrap();
        assert!(cs.container_id.is_none());
        assert!(cs.compose_project.is_none());
        assert!(cs.compose_service.is_none());
    }

    #[tokio::test]
    async fn test_down_non_compose_uses_stop_remove() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.down(&id).await.unwrap();

        // Should call stop + remove, NOT compose_down
        let recorded = calls.lock().unwrap();
        assert!(recorded.iter().any(|c| matches!(c, MockCall::Stop { .. })));
        assert!(recorded.iter().any(|c| matches!(c, MockCall::Remove { .. })));
        assert!(!recorded.iter().any(|c| matches!(c, MockCall::ComposeDown { .. })));
    }

    // ==================== Compose Start / Stop ====================

    /// Helper: create a compose workspace with devcontainer.json + docker-compose.yml
    fn create_compose_workspace() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let devcontainer_dir = tmp.path().join(".devcontainer");
        std::fs::create_dir_all(&devcontainer_dir).unwrap();
        std::fs::write(
            devcontainer_dir.join("devcontainer.json"),
            r#"{"dockerComposeFile": "docker-compose.yml", "service": "app", "workspaceFolder": "/workspace"}"#,
        )
        .unwrap();
        std::fs::write(
            devcontainer_dir.join("docker-compose.yml"),
            "version: '3'\nservices:\n  app:\n    image: ubuntu:22.04\n",
        )
        .unwrap();
        tmp
    }

    #[tokio::test]
    async fn test_compose_start_calls_compose_up_and_sets_container_id() {
        let workspace = create_compose_workspace();

        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();
        *mock.compose_ps_result.lock().unwrap() = Ok(vec![ComposeServiceInfo {
            service_name: "app".to_string(),
            container_id: ContainerId::new("compose_start_abc"),
            status: ContainerStatus::Running,
        }]);

        let mut state = StateStore::new();
        // Use Stopped status — can_start() requires Created or Stopped
        let mut cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Stopped,
            None,
            None,
        );
        cs.compose_project = Some("devc-test".to_string());
        cs.compose_service = Some("app".to_string());
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.start(&id).await.unwrap();

        // Verify compose_up and compose_ps were called
        let recorded = calls.lock().unwrap();
        assert!(recorded.iter().any(|c| matches!(c, MockCall::ComposeUp { .. })));
        assert!(recorded.iter().any(|c| matches!(c, MockCall::ComposePs { .. })));

        // Verify container_id was set from the matched service
        let cs = mgr.get(&id).await.unwrap().unwrap();
        assert_eq!(cs.container_id, Some("compose_start_abc".to_string()));
        assert_eq!(cs.status, DevcContainerStatus::Running);
    }

    #[tokio::test]
    async fn test_compose_start_service_not_found_returns_error() {
        let workspace = create_compose_workspace();

        let mock = MockProvider::new(ProviderType::Docker);
        // compose_ps returns a service that does NOT match the primary service "app"
        *mock.compose_ps_result.lock().unwrap() = Ok(vec![ComposeServiceInfo {
            service_name: "db".to_string(),
            container_id: ContainerId::new("compose_db_123"),
            status: ContainerStatus::Running,
        }]);

        let mut state = StateStore::new();
        let mut cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Stopped,
            None,
            None,
        );
        cs.compose_project = Some("devc-test".to_string());
        cs.compose_service = Some("app".to_string());
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let result = mgr.start(&id).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("not found"));
    }

    #[tokio::test]
    async fn test_compose_stop_calls_compose_down() {
        let workspace = create_compose_workspace();

        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let mut cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("compose"),
            Some("compose_container_456"),
        );
        cs.compose_project = Some("devc-test".to_string());
        cs.compose_service = Some("app".to_string());
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.stop(&id).await.unwrap();

        // Should call compose_down, NOT individual stop
        let recorded = calls.lock().unwrap();
        assert!(recorded.iter().any(|c| matches!(c, MockCall::ComposeDown { .. })));
        assert!(!recorded.iter().any(|c| matches!(c, MockCall::Stop { .. })));

        // container_id should be cleared
        let cs = mgr.get(&id).await.unwrap().unwrap();
        assert!(cs.container_id.is_none());
        assert_eq!(cs.status, DevcContainerStatus::Stopped);
    }

    #[tokio::test]
    async fn test_init_from_config_new() {
        let tmp = tempfile::tempdir().unwrap();
        let dc = tmp.path().join(".devcontainer");
        std::fs::create_dir_all(&dc).unwrap();
        let config_path = dc.join("devcontainer.json");
        std::fs::write(&config_path, r#"{"image": "ubuntu:22.04"}"#).unwrap();

        let mock = MockProvider::new(ProviderType::Docker);
        let mgr = test_manager(mock);

        let result = mgr.init_from_config(&config_path).await.unwrap();
        assert!(result.is_some());
        let cs = result.unwrap();
        assert_eq!(cs.status, DevcContainerStatus::Configured);
        assert_eq!(cs.config_path, config_path);
    }

    #[tokio::test]
    async fn test_init_from_config_duplicate_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let dc = tmp.path().join(".devcontainer");
        std::fs::create_dir_all(&dc).unwrap();
        let config_path = dc.join("devcontainer.json");
        std::fs::write(&config_path, r#"{"image": "ubuntu:22.04"}"#).unwrap();

        let mock = MockProvider::new(ProviderType::Docker);
        let mgr = test_manager(mock);

        // First call registers
        let first = mgr.init_from_config(&config_path).await.unwrap();
        assert!(first.is_some());

        // Second call returns None (already registered)
        let second = mgr.init_from_config(&config_path).await.unwrap();
        assert!(second.is_none());
    }

    #[tokio::test]
    async fn test_auto_discover_registers_all() {
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

        let mock = MockProvider::new(ProviderType::Docker);
        let mgr = test_manager(mock);

        let newly = mgr.auto_discover_configs(tmp.path()).await.unwrap();
        assert_eq!(newly.len(), 3);

        // All three should be in the list
        let all = mgr.list().await.unwrap();
        assert_eq!(all.len(), 3);
    }

    #[tokio::test]
    async fn test_auto_discover_skips_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let dc = tmp.path().join(".devcontainer");
        std::fs::create_dir_all(dc.join("python")).unwrap();
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

        let mock = MockProvider::new(ProviderType::Docker);
        let mgr = test_manager(mock);

        // First discovery registers both
        let first = mgr.auto_discover_configs(tmp.path()).await.unwrap();
        assert_eq!(first.len(), 2);

        // Second discovery registers none (already tracked)
        let second = mgr.auto_discover_configs(tmp.path()).await.unwrap();
        assert_eq!(second.len(), 0);

        // Total should still be 2
        let all = mgr.list().await.unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_merge_remote_env_both_empty() {
        let feature_env = HashMap::new();
        let result = merge_remote_env(None, &feature_env);
        assert!(result.is_none());
    }

    #[test]
    fn test_merge_remote_env_feature_only() {
        let mut feature_env = HashMap::new();
        feature_env.insert("FOO".to_string(), "bar".to_string());
        let result = merge_remote_env(None, &feature_env).unwrap();
        assert_eq!(result.get("FOO").unwrap(), "bar");
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_merge_remote_env_devcontainer_only() {
        let feature_env = HashMap::new();
        let mut dc_env = HashMap::new();
        dc_env.insert("EDITOR".to_string(), "vim".to_string());
        let result = merge_remote_env(Some(&dc_env), &feature_env).unwrap();
        assert_eq!(result.get("EDITOR").unwrap(), "vim");
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn test_merge_remote_env_devcontainer_wins() {
        let mut feature_env = HashMap::new();
        feature_env.insert("EDITOR".to_string(), "nano".to_string());
        feature_env.insert("FEATURE_VAR".to_string(), "hello".to_string());
        let mut dc_env = HashMap::new();
        dc_env.insert("EDITOR".to_string(), "vim".to_string());
        dc_env.insert("DC_VAR".to_string(), "world".to_string());
        let result = merge_remote_env(Some(&dc_env), &feature_env).unwrap();
        assert_eq!(result.get("EDITOR").unwrap(), "vim", "devcontainer.json should win");
        assert_eq!(result.get("FEATURE_VAR").unwrap(), "hello");
        assert_eq!(result.get("DC_VAR").unwrap(), "world");
        assert_eq!(result.len(), 3);
    }

    // ==================== Lifecycle Event Ordering ====================

    /// Create a workspace with all lifecycle commands configured.
    /// Returns (tempdir, marker_file_path) where marker_file_path is the
    /// file that initializeCommand will `touch` on the host.
    fn create_lifecycle_workspace() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("init_marker");
        let devcontainer_dir = tmp.path().join(".devcontainer");
        std::fs::create_dir_all(&devcontainer_dir).unwrap();
        let config = format!(
            r#"{{
                "image": "ubuntu:22.04",
                "initializeCommand": "touch {}",
                "onCreateCommand": "echo on-create",
                "updateContentCommand": "echo update-content",
                "postCreateCommand": "echo post-create",
                "postStartCommand": "echo post-start",
                "postAttachCommand": "echo post-attach"
            }}"#,
            marker.display()
        );
        std::fs::write(devcontainer_dir.join("devcontainer.json"), config).unwrap();
        (tmp, marker)
    }

    /// Build a manager with credentials and SSH disabled to keep lifecycle tests focused.
    fn test_manager_no_creds(mock: MockProvider, state: StateStore) -> ContainerManager {
        let mut global_config = GlobalConfig::default();
        global_config.credentials.docker = false;
        global_config.credentials.git = false;
        ContainerManager::new_for_testing(Box::new(mock), global_config, state)
    }

    /// Filter mock calls to only Exec calls, returning just the command vectors.
    fn exec_commands(calls: &[MockCall]) -> Vec<Vec<String>> {
        calls
            .iter()
            .filter_map(|c| {
                if let MockCall::Exec { cmd, .. } = c {
                    Some(cmd.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Extract the shell command string from an Exec call like ["/bin/sh", "-c", "echo foo"]
    fn shell_cmd(cmd: &[String]) -> &str {
        assert_eq!(cmd.len(), 3);
        assert_eq!(cmd[0], "/bin/sh");
        assert_eq!(cmd[1], "-c");
        &cmd[2]
    }

    #[tokio::test]
    async fn test_up_lifecycle_event_order() {
        let (workspace, marker) = create_lifecycle_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Configured,
            None,
            None,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.up(&id).await.unwrap();

        // initializeCommand ran on host
        assert!(marker.exists(), "initializeCommand marker file should exist");

        let recorded = calls.lock().unwrap();

        // initializeCommand ran before any provider call (marker file exists and
        // first provider call is Pull, confirming host command ran first)
        assert!(
            matches!(&recorded[0], MockCall::Pull { image } if image == "ubuntu:22.04"),
            "First provider call should be Pull; got {:?}",
            &recorded[0]
        );

        // Verify the provider call order: Pull → RemoveByName → Create → lifecycle execs → start phase
        let call_types: Vec<&str> = recorded
            .iter()
            .map(|c| match c {
                MockCall::Pull { .. } => "Pull",
                MockCall::Create { .. } => "Create",
                MockCall::Start { .. } => "Start",
                MockCall::Inspect { .. } => "Inspect",
                MockCall::Exec { .. } => "Exec",
                MockCall::RemoveByName { .. } => "RemoveByName",
                _ => "Other",
            })
            .collect();

        // Pull must come before Create
        let pull_idx = call_types.iter().position(|&t| t == "Pull").unwrap();
        let create_idx = call_types.iter().position(|&t| t == "Create").unwrap();
        assert!(pull_idx < create_idx, "Pull must come before Create");

        // Verify exec commands run in the correct lifecycle order
        let execs = exec_commands(&recorded);
        assert!(execs.len() >= 4, "Expected at least 4 exec calls (onCreate, updateContent, postCreate, postStart), got {}", execs.len());

        let lifecycle_cmds: Vec<&str> = execs.iter().map(|cmd| shell_cmd(cmd)).collect();
        let on_create_idx = lifecycle_cmds.iter().position(|&c| c == "echo on-create")
            .expect("onCreateCommand should have run");
        let update_content_idx = lifecycle_cmds.iter().position(|&c| c == "echo update-content")
            .expect("updateContentCommand should have run");
        let post_create_idx = lifecycle_cmds.iter().position(|&c| c == "echo post-create")
            .expect("postCreateCommand should have run");
        let post_start_idx = lifecycle_cmds.iter().position(|&c| c == "echo post-start")
            .expect("postStartCommand should have run");

        assert!(on_create_idx < update_content_idx,
            "onCreateCommand must run before updateContentCommand");
        assert!(update_content_idx < post_create_idx,
            "updateContentCommand must run before postCreateCommand");
        assert!(post_create_idx < post_start_idx,
            "postCreateCommand must run before postStartCommand");

        // postAttachCommand must NOT run during up
        assert!(
            !lifecycle_cmds.iter().any(|&c| c == "echo post-attach"),
            "postAttachCommand should NOT run during up"
        );

        // All execs happen after Create
        let first_exec_overall = recorded.iter().position(|c| matches!(c, MockCall::Exec { .. })).unwrap();
        assert!(create_idx < first_exec_overall, "Create must come before any Exec");
    }

    #[tokio::test]
    async fn test_rebuild_runs_initialize_before_build() {
        let (workspace, marker) = create_lifecycle_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:old_image"),
            Some("old_container_123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.rebuild(&id, false).await.unwrap();

        // initializeCommand marker must exist
        assert!(marker.exists(), "initializeCommand marker file should exist");

        let recorded = calls.lock().unwrap();

        // Down phase (Stop + Remove) must come before Pull (build)
        let stop_idx = recorded.iter().position(|c| matches!(c, MockCall::Stop { .. })).unwrap();
        let remove_idx = recorded.iter().position(|c| matches!(c, MockCall::Remove { .. })).unwrap();
        let pull_idx = recorded.iter().position(|c| matches!(c, MockCall::Pull { .. })).unwrap();

        assert!(stop_idx < pull_idx, "Stop must come before Pull (build)");
        assert!(remove_idx < pull_idx, "Remove must come before Pull (build)");
    }

    #[tokio::test]
    async fn test_rebuild_with_progress_runs_initialize_before_build() {
        let (workspace, marker) = create_lifecycle_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:old_image"),
            Some("old_container_123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        let (tx, _rx) = mpsc::unbounded_channel();
        mgr.rebuild_with_progress(&id, false, tx).await.unwrap();

        // initializeCommand marker must exist
        assert!(marker.exists(), "initializeCommand marker file should exist");

        let recorded = calls.lock().unwrap();

        // Down phase (Stop + Remove) must come before Pull (build)
        let stop_idx = recorded.iter().position(|c| matches!(c, MockCall::Stop { .. })).unwrap();
        let remove_idx = recorded.iter().position(|c| matches!(c, MockCall::Remove { .. })).unwrap();
        let pull_idx = recorded.iter().position(|c| matches!(c, MockCall::Pull { .. })).unwrap();

        assert!(stop_idx < pull_idx, "Stop must come before Pull (build)");
        assert!(remove_idx < pull_idx, "Remove must come before Pull (build)");
    }

    #[tokio::test]
    async fn test_rebuild_full_lifecycle_order() {
        let (workspace, marker) = create_lifecycle_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:old_image"),
            Some("old_container_123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.rebuild(&id, false).await.unwrap();

        assert!(marker.exists(), "initializeCommand marker file should exist");

        let recorded = calls.lock().unwrap();

        // Phase 1: Down
        let stop_idx = recorded.iter().position(|c| matches!(c, MockCall::Stop { .. })).unwrap();
        let remove_idx = recorded.iter().position(|c| matches!(c, MockCall::Remove { .. })).unwrap();

        // Phase 2: Build (Pull)
        let pull_idx = recorded.iter().position(|c| matches!(c, MockCall::Pull { .. })).unwrap();

        // Phase 3: Create
        let create_idx = recorded.iter().position(|c| matches!(c, MockCall::Create { .. })).unwrap();

        // Verify overall phase ordering: Down → Build → Create → lifecycle execs
        assert!(stop_idx < remove_idx, "Stop before Remove");
        assert!(remove_idx < pull_idx, "Remove before Pull");
        assert!(pull_idx < create_idx, "Pull before Create");

        // Verify exec commands run in correct lifecycle order
        let execs = exec_commands(&recorded);
        let lifecycle_cmds: Vec<&str> = execs.iter().map(|cmd| shell_cmd(cmd)).collect();

        let on_create_idx = lifecycle_cmds.iter().position(|&c| c == "echo on-create")
            .expect("onCreateCommand should have run");
        let update_content_idx = lifecycle_cmds.iter().position(|&c| c == "echo update-content")
            .expect("updateContentCommand should have run");
        let post_create_idx = lifecycle_cmds.iter().position(|&c| c == "echo post-create")
            .expect("postCreateCommand should have run");
        let post_start_idx = lifecycle_cmds.iter().position(|&c| c == "echo post-start")
            .expect("postStartCommand should have run");

        assert!(on_create_idx < update_content_idx, "onCreate before updateContent");
        assert!(update_content_idx < post_create_idx, "updateContent before postCreate");
        assert!(post_create_idx < post_start_idx, "postCreate before postStart");

        // postAttachCommand must NOT run during rebuild
        assert!(
            !lifecycle_cmds.iter().any(|&c| c == "echo post-attach"),
            "postAttachCommand should NOT run during rebuild"
        );

        // All execs must come after Create
        let first_exec_overall = recorded.iter().position(|c| matches!(c, MockCall::Exec { .. })).unwrap();
        assert!(create_idx < first_exec_overall, "Create must come before any Exec");
    }

    #[tokio::test]
    async fn test_start_runs_post_start_not_on_create() {
        let (workspace, _marker) = create_lifecycle_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        // Stopped container — already existed, not freshly Created
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Stopped,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.start(&id).await.unwrap();

        let recorded = calls.lock().unwrap();
        let execs = exec_commands(&recorded);
        let lifecycle_cmds: Vec<&str> = execs.iter().map(|cmd| shell_cmd(cmd)).collect();

        // postStartCommand should run
        assert!(
            lifecycle_cmds.contains(&"echo post-start"),
            "postStartCommand should run on start; got {:?}",
            lifecycle_cmds
        );

        // onCreateCommand should NOT run (container was Stopped, not Created)
        assert!(
            !lifecycle_cmds.contains(&"echo on-create"),
            "onCreateCommand should NOT run on start of existing container"
        );

        // updateContentCommand and postCreateCommand should NOT run either
        assert!(
            !lifecycle_cmds.contains(&"echo update-content"),
            "updateContentCommand should NOT run on start"
        );
        assert!(
            !lifecycle_cmds.contains(&"echo post-create"),
            "postCreateCommand should NOT run on start"
        );

        // postAttachCommand should NOT run during start
        assert!(
            !lifecycle_cmds.contains(&"echo post-attach"),
            "postAttachCommand should NOT run on start"
        );
    }

    #[tokio::test]
    async fn test_post_attach_command_runs() {
        let (workspace, _marker) = create_lifecycle_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.run_post_attach_command(&id).await.unwrap();

        let recorded = calls.lock().unwrap();
        let execs = exec_commands(&recorded);
        let lifecycle_cmds: Vec<&str> = execs.iter().map(|cmd| shell_cmd(cmd)).collect();

        assert!(
            lifecycle_cmds.contains(&"echo post-attach"),
            "postAttachCommand should have run; got {:?}",
            lifecycle_cmds
        );

        // No other lifecycle commands should run
        assert!(!lifecycle_cmds.contains(&"echo on-create"));
        assert!(!lifecycle_cmds.contains(&"echo update-content"));
        assert!(!lifecycle_cmds.contains(&"echo post-create"));
        assert!(!lifecycle_cmds.contains(&"echo post-start"));
    }

    #[tokio::test]
    async fn test_up_on_create_only_on_first_create() {
        let (workspace, _marker) = create_lifecycle_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        // Start as Configured with no image/container — will go through full up flow
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Configured,
            None,
            None,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);

        // First up: full lifecycle including onCreate
        mgr.up(&id).await.unwrap();

        let recorded_after_up = calls.lock().unwrap().clone();
        let execs_after_up = exec_commands(&recorded_after_up);
        let cmds_after_up: Vec<&str> = execs_after_up.iter().map(|cmd| shell_cmd(cmd)).collect();

        assert!(cmds_after_up.contains(&"echo on-create"), "onCreate should run on first up");
        assert!(cmds_after_up.contains(&"echo post-create"), "postCreate should run on first up");

        let on_create_count = cmds_after_up.iter().filter(|&&c| c == "echo on-create").count();
        assert_eq!(on_create_count, 1, "onCreateCommand should run exactly once during up");

        // Now call start — should NOT re-run onCreate/postCreate
        calls.lock().unwrap().clear();
        mgr.start(&id).await.unwrap();

        let recorded_after_start = calls.lock().unwrap().clone();
        let execs_after_start = exec_commands(&recorded_after_start);
        let cmds_after_start: Vec<&str> = execs_after_start.iter().map(|cmd| shell_cmd(cmd)).collect();

        assert!(
            !cmds_after_start.contains(&"echo on-create"),
            "onCreateCommand should NOT run on subsequent start"
        );
        assert!(
            !cmds_after_start.contains(&"echo post-create"),
            "postCreateCommand should NOT run on subsequent start"
        );
        // postStartCommand should still run
        assert!(
            cmds_after_start.contains(&"echo post-start"),
            "postStartCommand should run on start"
        );
    }

    // ==================== New Lifecycle Tests ====================

    #[tokio::test]
    async fn test_stop_runs_no_lifecycle_commands() {
        let (workspace, _marker) = create_lifecycle_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.stop(&id).await.unwrap();

        let recorded = calls.lock().unwrap();
        let execs = exec_commands(&recorded);
        assert!(execs.is_empty(), "stop should not run any lifecycle Exec calls, got {:?}", execs);
    }

    #[tokio::test]
    async fn test_down_runs_no_lifecycle_commands() {
        let (workspace, _marker) = create_lifecycle_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.down(&id).await.unwrap();

        let recorded = calls.lock().unwrap();
        let execs = exec_commands(&recorded);
        assert!(execs.is_empty(), "down should not run any lifecycle Exec calls, got {:?}", execs);
    }

    #[tokio::test]
    async fn test_up_on_running_container_skips_create_phase() {
        let (workspace, marker) = create_lifecycle_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.up(&id).await.unwrap();

        // initializeCommand should NOT run (image already built)
        assert!(!marker.exists(), "initializeCommand should NOT run on already-running container");

        let recorded = calls.lock().unwrap();

        // No Build/Pull/Create calls
        assert!(
            !recorded.iter().any(|c| matches!(c, MockCall::Pull { .. } | MockCall::Build { .. } | MockCall::Create { .. })),
            "Should not build/pull/create for already-running container"
        );

        // No onCreate, updateContent, postCreate
        let execs = exec_commands(&recorded);
        let cmds: Vec<&str> = execs.iter().map(|cmd| shell_cmd(cmd)).collect();
        assert!(!cmds.contains(&"echo on-create"), "onCreate should NOT run");
        assert!(!cmds.contains(&"echo update-content"), "updateContent should NOT run");
        assert!(!cmds.contains(&"echo post-create"), "postCreate should NOT run");

        // postStart should run
        assert!(cmds.contains(&"echo post-start"), "postStart should run");
    }

    #[tokio::test]
    async fn test_up_on_stopped_container_skips_create_phase() {
        let (workspace, marker) = create_lifecycle_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Stopped,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.up(&id).await.unwrap();

        // initializeCommand should NOT run (image already built)
        assert!(!marker.exists(), "initializeCommand should NOT run on stopped container with image");

        let recorded = calls.lock().unwrap();

        // No Build/Pull/Create
        assert!(
            !recorded.iter().any(|c| matches!(c, MockCall::Pull { .. } | MockCall::Build { .. } | MockCall::Create { .. })),
            "Should not build/pull/create for stopped container with existing image"
        );

        // No onCreate, updateContent, postCreate
        let execs = exec_commands(&recorded);
        let cmds: Vec<&str> = execs.iter().map(|cmd| shell_cmd(cmd)).collect();
        assert!(!cmds.contains(&"echo on-create"), "onCreate should NOT run");
        assert!(!cmds.contains(&"echo update-content"), "updateContent should NOT run");
        assert!(!cmds.contains(&"echo post-create"), "postCreate should NOT run");

        // postStart should run (via start phase)
        assert!(cmds.contains(&"echo post-start"), "postStart should run on stopped->running");
    }

    #[tokio::test]
    async fn test_up_initialize_command_only_before_build() {
        let (workspace, marker) = create_lifecycle_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Configured,
            None,
            None,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);

        // First up: marker should be created (initializeCommand runs before build)
        mgr.up(&id).await.unwrap();
        assert!(marker.exists(), "initializeCommand should create marker on first up");

        // Delete marker and call up again (container already has image + container_id)
        std::fs::remove_file(&marker).unwrap();
        mgr.up(&id).await.unwrap();

        // Marker should NOT be recreated (initializeCommand skipped on subsequent up)
        assert!(!marker.exists(), "initializeCommand should NOT run on subsequent up");
    }

    /// Create a workspace with devcontainer.json commands prefixed "dc-" and
    /// feature properties with commands prefixed "feat-", stored in metadata.
    fn create_lifecycle_workspace_with_features() -> (tempfile::TempDir, std::path::PathBuf, String) {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("init_marker");
        let devcontainer_dir = tmp.path().join(".devcontainer");
        std::fs::create_dir_all(&devcontainer_dir).unwrap();
        let config = format!(
            r#"{{
                "image": "ubuntu:22.04",
                "initializeCommand": "touch {}",
                "onCreateCommand": "echo dc-on-create",
                "updateContentCommand": "echo dc-update-content",
                "postCreateCommand": "echo dc-post-create",
                "postStartCommand": "echo dc-post-start",
                "postAttachCommand": "echo dc-post-attach",
                "remoteUser": "devuser",
                "workspaceFolder": "/workspace/project"
            }}"#,
            marker.display()
        );
        std::fs::write(devcontainer_dir.join("devcontainer.json"), &config).unwrap();

        // Build feature properties JSON
        let feature_props = crate::features::MergedFeatureProperties {
            on_create_commands: vec![devc_config::Command::String("echo feat-on-create".to_string())],
            update_content_commands: vec![devc_config::Command::String("echo feat-update-content".to_string())],
            post_create_commands: vec![devc_config::Command::String("echo feat-post-create".to_string())],
            post_start_commands: vec![devc_config::Command::String("echo feat-post-start".to_string())],
            post_attach_commands: vec![devc_config::Command::String("echo feat-post-attach".to_string())],
            ..Default::default()
        };
        let feature_json = serde_json::to_string(&feature_props).unwrap();

        (tmp, marker, feature_json)
    }

    fn make_container_state_with_features(
        workspace: &std::path::Path,
        status: DevcContainerStatus,
        image_id: Option<&str>,
        container_id: Option<&str>,
        feature_json: &str,
    ) -> ContainerState {
        let mut cs = make_container_state(workspace, status, image_id, container_id);
        cs.metadata.insert("feature_properties".to_string(), feature_json.to_string());
        cs
    }

    #[tokio::test]
    async fn test_up_feature_lifecycle_before_devcontainer() {
        let (workspace, _marker, feature_json) = create_lifecycle_workspace_with_features();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        // Start at Built with image_id set so build is skipped (build would overwrite
        // our manually-set feature_properties). No container_id so create runs, then
        // lifecycle commands execute using our feature_properties from metadata.
        let cs = make_container_state_with_features(
            workspace.path(),
            DevcContainerStatus::Built,
            Some("sha256:mock_image_id"),
            None,
            &feature_json,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.up(&id).await.unwrap();

        let recorded = calls.lock().unwrap();
        let execs = exec_commands(&recorded);
        let cmds: Vec<&str> = execs.iter().map(|cmd| shell_cmd(cmd)).collect();

        // Feature onCreate before devcontainer onCreate
        let feat_on_create = cmds.iter().position(|&c| c == "echo feat-on-create")
            .expect("feature onCreateCommand should run");
        let dc_on_create = cmds.iter().position(|&c| c == "echo dc-on-create")
            .expect("devcontainer onCreateCommand should run");
        assert!(feat_on_create < dc_on_create, "feature onCreate should run before dc onCreate");

        // Feature updateContent before devcontainer updateContent
        let feat_update = cmds.iter().position(|&c| c == "echo feat-update-content")
            .expect("feature updateContentCommand should run");
        let dc_update = cmds.iter().position(|&c| c == "echo dc-update-content")
            .expect("devcontainer updateContentCommand should run");
        assert!(feat_update < dc_update, "feature updateContent should run before dc updateContent");

        // Feature postCreate before devcontainer postCreate
        let feat_post_create = cmds.iter().position(|&c| c == "echo feat-post-create")
            .expect("feature postCreateCommand should run");
        let dc_post_create = cmds.iter().position(|&c| c == "echo dc-post-create")
            .expect("devcontainer postCreateCommand should run");
        assert!(feat_post_create < dc_post_create, "feature postCreate should run before dc postCreate");

        // Feature postStart before devcontainer postStart
        let feat_post_start = cmds.iter().position(|&c| c == "echo feat-post-start")
            .expect("feature postStartCommand should run");
        let dc_post_start = cmds.iter().position(|&c| c == "echo dc-post-start")
            .expect("devcontainer postStartCommand should run");
        assert!(feat_post_start < dc_post_start, "feature postStart should run before dc postStart");
    }

    #[tokio::test]
    async fn test_start_feature_post_start_before_devcontainer() {
        let (workspace, _marker, feature_json) = create_lifecycle_workspace_with_features();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state_with_features(
            workspace.path(),
            DevcContainerStatus::Stopped,
            Some("sha256:img"),
            Some("container123"),
            &feature_json,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.start(&id).await.unwrap();

        let recorded = calls.lock().unwrap();
        let execs = exec_commands(&recorded);
        let cmds: Vec<&str> = execs.iter().map(|cmd| shell_cmd(cmd)).collect();

        let feat_idx = cmds.iter().position(|&c| c == "echo feat-post-start")
            .expect("feature postStartCommand should run");
        let dc_idx = cmds.iter().position(|&c| c == "echo dc-post-start")
            .expect("devcontainer postStartCommand should run");
        assert!(feat_idx < dc_idx, "feature postStart should run before dc postStart");
    }

    #[tokio::test]
    async fn test_post_attach_feature_before_devcontainer() {
        let (workspace, _marker, feature_json) = create_lifecycle_workspace_with_features();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state_with_features(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
            &feature_json,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.run_post_attach_command(&id).await.unwrap();

        let recorded = calls.lock().unwrap();
        let execs = exec_commands(&recorded);
        let cmds: Vec<&str> = execs.iter().map(|cmd| shell_cmd(cmd)).collect();

        let feat_idx = cmds.iter().position(|&c| c == "echo feat-post-attach")
            .expect("feature postAttachCommand should run");
        let dc_idx = cmds.iter().position(|&c| c == "echo dc-post-attach")
            .expect("devcontainer postAttachCommand should run");
        assert!(feat_idx < dc_idx, "feature postAttach should run before dc postAttach");
    }

    #[tokio::test]
    async fn test_lifecycle_commands_use_workspace_folder() {
        let (workspace, _marker, feature_json) = create_lifecycle_workspace_with_features();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state_with_features(
            workspace.path(),
            DevcContainerStatus::Built,
            Some("sha256:mock_image_id"),
            None,
            &feature_json,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.up(&id).await.unwrap();

        let recorded = calls.lock().unwrap();
        // All lifecycle Exec calls should have working_dir set to /workspace/project
        for call in recorded.iter() {
            if let MockCall::Exec { working_dir, .. } = call {
                assert_eq!(
                    working_dir.as_deref(),
                    Some("/workspace/project"),
                    "lifecycle Exec should use workspaceFolder as working_dir"
                );
            }
        }
    }

    #[tokio::test]
    async fn test_lifecycle_commands_use_remote_user() {
        let (workspace, _marker, feature_json) = create_lifecycle_workspace_with_features();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state_with_features(
            workspace.path(),
            DevcContainerStatus::Built,
            Some("sha256:mock_image_id"),
            None,
            &feature_json,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.up(&id).await.unwrap();

        let recorded = calls.lock().unwrap();
        // All lifecycle Exec calls should have user set to "devuser"
        for call in recorded.iter() {
            if let MockCall::Exec { user, .. } = call {
                assert_eq!(
                    user.as_deref(),
                    Some("devuser"),
                    "lifecycle Exec should use remoteUser"
                );
            }
        }
    }

    // ==================== Duplicate Name Lookup ====================

    #[tokio::test]
    async fn test_get_by_id_unambiguous_with_duplicate_names() {
        let tmp1 = create_test_workspace();
        let tmp2 = tempfile::tempdir().unwrap();
        let devcontainer_dir = tmp2.path().join(".devcontainer");
        std::fs::create_dir_all(&devcontainer_dir).unwrap();
        std::fs::write(
            devcontainer_dir.join("devcontainer.json"),
            r#"{"image": "ubuntu:22.04"}"#,
        )
        .unwrap();

        // Two containers with the same name but different workspaces (and thus different IDs)
        let mut cs1 = make_container_state(
            tmp1.path(),
            DevcContainerStatus::Running,
            Some("img1"),
            Some("cid1"),
        );
        cs1.name = "myproject".to_string();

        let mut cs2 = make_container_state(
            tmp2.path(),
            DevcContainerStatus::Running,
            Some("img2"),
            Some("cid2"),
        );
        cs2.name = "myproject".to_string();

        // Ensure different IDs
        assert_ne!(cs1.id, cs2.id);

        let id1 = cs1.id.clone();
        let id2 = cs2.id.clone();

        let mut state = StateStore::new();
        state.add(cs1);
        state.add(cs2);

        let mock = MockProvider::new(ProviderType::Docker);
        let mgr = test_manager_with_state(mock, state);

        // get() by ID always returns the exact container
        let found1 = mgr.get(&id1).await.unwrap().unwrap();
        assert_eq!(found1.id, id1);
        assert_eq!(found1.container_id.as_deref(), Some("cid1"));

        let found2 = mgr.get(&id2).await.unwrap().unwrap();
        assert_eq!(found2.id, id2);
        assert_eq!(found2.container_id.as_deref(), Some("cid2"));
    }

    #[tokio::test]
    async fn test_get_by_name_returns_one_when_duplicates() {
        let tmp1 = create_test_workspace();
        let tmp2 = tempfile::tempdir().unwrap();
        let devcontainer_dir = tmp2.path().join(".devcontainer");
        std::fs::create_dir_all(&devcontainer_dir).unwrap();
        std::fs::write(
            devcontainer_dir.join("devcontainer.json"),
            r#"{"image": "ubuntu:22.04"}"#,
        )
        .unwrap();

        let mut cs1 = make_container_state(
            tmp1.path(),
            DevcContainerStatus::Running,
            Some("img1"),
            Some("cid1"),
        );
        cs1.name = "myproject".to_string();

        let mut cs2 = make_container_state(
            tmp2.path(),
            DevcContainerStatus::Running,
            Some("img2"),
            Some("cid2"),
        );
        cs2.name = "myproject".to_string();

        let mut state = StateStore::new();
        state.add(cs1);
        state.add(cs2);

        let mock = MockProvider::new(ProviderType::Docker);
        let mgr = test_manager_with_state(mock, state);

        // get_by_name returns *some* container — the result is non-deterministic
        // when there are duplicates, but it shouldn't error
        let found = mgr.get_by_name("myproject").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "myproject");
    }

    // ==================== Cross-provider ====================

    #[tokio::test]
    async fn test_cross_provider_uses_container_provider() {
        // Manager's default is Podman, but container was created with Docker
        let workspace = create_test_workspace();

        let podman_mock = MockProvider::new(ProviderType::Podman);
        let docker_mock = MockProvider::new(ProviderType::Docker);
        let docker_calls = docker_mock.calls.clone();
        let podman_calls = podman_mock.calls.clone();

        // Set up Docker mock with inspect result so sync_status works
        *docker_mock.inspect_result.lock().unwrap() =
            Ok(mock_container_details("docker_ctr_123", ContainerStatus::Running));

        let mut state = StateStore::new();
        let mut cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("img123"),
            Some("docker_ctr_123"),
        );
        cs.provider = ProviderType::Docker; // Container belongs to Docker
        let id = cs.id.clone();
        state.add(cs);

        let mgr = ContainerManager::new_for_testing_multi(
            vec![Box::new(podman_mock), Box::new(docker_mock)],
            ProviderType::Podman, // Default is Podman
            GlobalConfig::default(),
            state,
        );

        // sync_status should use the Docker provider (the container's provider)
        let status = mgr.sync_status(&id).await.unwrap();
        assert_eq!(status, DevcContainerStatus::Running);

        // Docker mock should have been called with Inspect, not the Podman mock
        let docker_recorded = docker_calls.lock().unwrap();
        assert!(
            docker_recorded.iter().any(|c| matches!(c, MockCall::Inspect { .. })),
            "Docker provider should have been used for inspect"
        );
        let podman_recorded = podman_calls.lock().unwrap();
        assert!(
            !podman_recorded.iter().any(|c| matches!(c, MockCall::Inspect { .. })),
            "Podman provider should NOT have been used for inspect"
        );
    }

    #[tokio::test]
    async fn test_cross_provider_stop_uses_container_provider() {
        // Container belongs to Docker but default provider is Podman
        let workspace = create_test_workspace();

        let podman_mock = MockProvider::new(ProviderType::Podman);
        let docker_mock = MockProvider::new(ProviderType::Docker);
        let docker_calls = docker_mock.calls.clone();
        let podman_calls = podman_mock.calls.clone();

        let mut state = StateStore::new();
        let mut cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("img123"),
            Some("docker_ctr_456"),
        );
        cs.provider = ProviderType::Docker;
        let id = cs.id.clone();
        state.add(cs);

        let mgr = ContainerManager::new_for_testing_multi(
            vec![Box::new(podman_mock), Box::new(docker_mock)],
            ProviderType::Podman,
            GlobalConfig::default(),
            state,
        );

        // stop() should route to Docker provider
        mgr.stop(&id).await.unwrap();

        let docker_recorded = docker_calls.lock().unwrap();
        assert!(
            docker_recorded.iter().any(|c| matches!(c, MockCall::Stop { .. })),
            "Docker provider should have been used for stop"
        );
        let podman_recorded = podman_calls.lock().unwrap();
        assert!(
            !podman_recorded.iter().any(|c| matches!(c, MockCall::Stop { .. })),
            "Podman provider should NOT have been used for stop"
        );
    }

    // ==================== Adopt — remote_user metadata ====================

    /// Helper: create workspace with remoteUser in devcontainer.json
    fn create_test_workspace_with_user(user: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let devcontainer_dir = tmp.path().join(".devcontainer");
        std::fs::create_dir_all(&devcontainer_dir).unwrap();
        std::fs::write(
            devcontainer_dir.join("devcontainer.json"),
            format!(r#"{{"image": "ubuntu:22.04", "remoteUser": "{}"}}"#, user),
        )
        .unwrap();
        tmp
    }

    #[tokio::test]
    async fn test_adopt_stores_remote_user() {
        let workspace = create_test_workspace_with_user("vscode");
        let mock = MockProvider::new(ProviderType::Docker);
        let mgr = test_manager(mock);
        let state = mgr
            .adopt(
                "mock_container_id",
                Some(workspace.path().to_str().unwrap()),
                DevcontainerSource::VsCode,
                ProviderType::Docker,
            )
            .await
            .unwrap();
        assert_eq!(
            state.metadata.get("remote_user").map(|s| s.as_str()),
            Some("vscode"),
        );
    }

    #[tokio::test]
    async fn test_adopt_no_remote_user_when_not_configured() {
        let workspace = create_test_workspace(); // no remoteUser
        let mock = MockProvider::new(ProviderType::Docker);
        let mgr = test_manager(mock);
        let state = mgr
            .adopt(
                "mock_container_id",
                Some(workspace.path().to_str().unwrap()),
                DevcontainerSource::VsCode,
                ProviderType::Docker,
            )
            .await
            .unwrap();
        assert!(
            state.metadata.get("remote_user").is_none(),
            "Should not have remote_user when devcontainer.json doesn't specify one"
        );
    }

    // ==================== Adopt — workspace_folder metadata ====================

    /// Helper: create workspace with workspaceFolder in devcontainer.json
    fn create_test_workspace_with_workspace_folder(folder: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let devcontainer_dir = tmp.path().join(".devcontainer");
        std::fs::create_dir_all(&devcontainer_dir).unwrap();
        std::fs::write(
            devcontainer_dir.join("devcontainer.json"),
            format!(r#"{{"image": "ubuntu:22.04", "workspaceFolder": "{}"}}"#, folder),
        )
        .unwrap();
        tmp
    }

    #[tokio::test]
    async fn test_adopt_stores_workspace_folder() {
        let workspace = create_test_workspace_with_workspace_folder("/workspaces/myapp");
        let mock = MockProvider::new(ProviderType::Docker);
        let mgr = test_manager(mock);
        let state = mgr
            .adopt(
                "mock_container_id",
                Some(workspace.path().to_str().unwrap()),
                DevcontainerSource::VsCode,
                ProviderType::Docker,
            )
            .await
            .unwrap();
        assert_eq!(
            state.metadata.get("workspace_folder").map(|s| s.as_str()),
            Some("/workspaces/myapp"),
        );
    }

    #[tokio::test]
    async fn test_adopt_infers_workspace_from_mounts() {
        let workspace = create_test_workspace(); // no workspaceFolder in config
        let mock = MockProvider::new(ProviderType::Docker);
        // Set up inspect result with a bind mount to /workspaces/project
        let mut details = mock_container_details("mock_container_id", ContainerStatus::Running);
        details.mounts.push(devc_provider::MountInfo {
            mount_type: "bind".to_string(),
            source: "/home/user/project".to_string(),
            destination: "/workspaces/project".to_string(),
            read_only: false,
        });
        *mock.inspect_result.lock().unwrap() = Ok(details);
        let mgr = test_manager(mock);
        let state = mgr
            .adopt(
                "mock_container_id",
                Some(workspace.path().to_str().unwrap()),
                DevcontainerSource::VsCode,
                ProviderType::Docker,
            )
            .await
            .unwrap();
        assert_eq!(
            state.metadata.get("workspace_folder").map(|s| s.as_str()),
            Some("/workspaces/project"),
        );
    }

    #[tokio::test]
    async fn test_adopt_config_workspace_folder_beats_mounts() {
        let workspace = create_test_workspace_with_workspace_folder("/workspaces/from-config");
        let mock = MockProvider::new(ProviderType::Docker);
        // Also set up a bind mount with a different path
        let mut details = mock_container_details("mock_container_id", ContainerStatus::Running);
        details.mounts.push(devc_provider::MountInfo {
            mount_type: "bind".to_string(),
            source: "/home/user/project".to_string(),
            destination: "/workspaces/from-mount".to_string(),
            read_only: false,
        });
        *mock.inspect_result.lock().unwrap() = Ok(details);
        let mgr = test_manager(mock);
        let state = mgr
            .adopt(
                "mock_container_id",
                Some(workspace.path().to_str().unwrap()),
                DevcontainerSource::VsCode,
                ProviderType::Docker,
            )
            .await
            .unwrap();
        assert_eq!(
            state.metadata.get("workspace_folder").map(|s| s.as_str()),
            Some("/workspaces/from-config"),
            "Config workspaceFolder should take precedence over bind mount detection"
        );
    }

    #[tokio::test]
    async fn test_adopt_no_workspace_folder_when_unavailable() {
        let workspace = create_test_workspace(); // no workspaceFolder, no bind mounts
        let mock = MockProvider::new(ProviderType::Docker);
        let mgr = test_manager(mock);
        let state = mgr
            .adopt(
                "mock_container_id",
                Some(workspace.path().to_str().unwrap()),
                DevcontainerSource::VsCode,
                ProviderType::Docker,
            )
            .await
            .unwrap();
        assert!(
            state.metadata.get("workspace_folder").is_none(),
            "Should not have workspace_folder when neither config nor mounts provide one"
        );
    }

    // ==================== Adopt — lifecycle commands ====================

    #[tokio::test]
    async fn test_adopt_runs_lifecycle_when_running() {
        let tmp = tempfile::tempdir().unwrap();
        let devcontainer_dir = tmp.path().join(".devcontainer");
        std::fs::create_dir_all(&devcontainer_dir).unwrap();
        std::fs::write(
            devcontainer_dir.join("devcontainer.json"),
            r#"{"image": "ubuntu:22.04", "postCreateCommand": "echo hello"}"#,
        )
        .unwrap();

        let mock = MockProvider::new(ProviderType::Docker);
        let calls = Arc::clone(&mock.calls);
        let mgr = test_manager(mock);

        let _state = mgr
            .adopt(
                "mock_container_id",
                Some(tmp.path().to_str().unwrap()),
                DevcontainerSource::VsCode,
                ProviderType::Docker,
            )
            .await
            .unwrap();

        // Should have made exec calls for lifecycle commands
        let recorded = calls.lock().unwrap();
        assert!(
            recorded.iter().any(|c| matches!(c, MockCall::Exec { .. })),
            "Expected lifecycle exec calls for running container, got: {:?}",
            *recorded,
        );
    }

    #[tokio::test]
    async fn test_adopt_skips_lifecycle_when_stopped() {
        let tmp = tempfile::tempdir().unwrap();
        let devcontainer_dir = tmp.path().join(".devcontainer");
        std::fs::create_dir_all(&devcontainer_dir).unwrap();
        std::fs::write(
            devcontainer_dir.join("devcontainer.json"),
            r#"{"image": "ubuntu:22.04", "postCreateCommand": "echo hello"}"#,
        )
        .unwrap();

        let mock = MockProvider::new(ProviderType::Docker);
        // Container is stopped
        *mock.inspect_result.lock().unwrap() =
            Ok(mock_container_details("mock_container_id", ContainerStatus::Exited));
        let calls = Arc::clone(&mock.calls);
        let mgr = test_manager(mock);

        let _state = mgr
            .adopt(
                "mock_container_id",
                Some(tmp.path().to_str().unwrap()),
                DevcontainerSource::VsCode,
                ProviderType::Docker,
            )
            .await
            .unwrap();

        // Should NOT have made exec calls (only inspect)
        let recorded = calls.lock().unwrap();
        assert!(
            !recorded.iter().any(|c| matches!(c, MockCall::Exec { .. })),
            "Should not exec lifecycle for stopped container, got: {:?}",
            *recorded,
        );
    }

    #[tokio::test]
    async fn test_adopt_lifecycle_failure_is_non_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let devcontainer_dir = tmp.path().join(".devcontainer");
        std::fs::create_dir_all(&devcontainer_dir).unwrap();
        std::fs::write(
            devcontainer_dir.join("devcontainer.json"),
            r#"{"image": "ubuntu:22.04", "postCreateCommand": "fail"}"#,
        )
        .unwrap();

        let mock = MockProvider::new(ProviderType::Docker);
        *mock.exec_error.lock().unwrap() =
            Some(ProviderError::ExecError("command failed".to_string()));
        let mgr = test_manager(mock);

        // adopt should succeed even when lifecycle commands fail
        let result = mgr
            .adopt(
                "mock_container_id",
                Some(tmp.path().to_str().unwrap()),
                DevcontainerSource::VsCode,
                ProviderType::Docker,
            )
            .await;
        assert!(result.is_ok(), "Adopt should succeed even when lifecycle fails");
    }

    // ==================== Delete safety for adopted containers ====================

    #[tokio::test]
    async fn test_remove_adopted_skips_runtime_destroy() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = Arc::clone(&mock.calls);

        // Create a state with an adopted container
        let mut state = StateStore::new();
        let mut cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("adopted_container_id"),
        );
        cs.source = DevcontainerSource::VsCode;
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.remove(&id, true).await.unwrap();

        let recorded = calls.lock().unwrap();
        assert!(
            !recorded.iter().any(|c| matches!(c, MockCall::Remove { .. })),
            "Should NOT call provider.remove() for adopted container, got: {:?}",
            *recorded,
        );

        // Verify removed from state
        let state = mgr.state.read().await;
        assert!(state.get(&id).is_none(), "Should be removed from state tracking");
    }

    #[tokio::test]
    async fn test_remove_devc_destroys_runtime() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = Arc::clone(&mock.calls);

        let mut state = StateStore::new();
        let mut cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("devc_container_id"),
        );
        cs.source = DevcontainerSource::Devc;
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.remove(&id, true).await.unwrap();

        let recorded = calls.lock().unwrap();
        assert!(
            recorded.iter().any(|c| matches!(c, MockCall::Remove { .. })),
            "Should call provider.remove() for devc-created container, got: {:?}",
            *recorded,
        );
    }

    #[tokio::test]
    async fn test_down_adopted_skips_runtime_destroy() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = Arc::clone(&mock.calls);

        let mut state = StateStore::new();
        let mut cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("adopted_container_id"),
        );
        cs.source = DevcontainerSource::VsCode;
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.down(&id).await.unwrap();

        let recorded = calls.lock().unwrap();
        assert!(
            !recorded.iter().any(|c| matches!(c, MockCall::Stop { .. })),
            "Should NOT call provider.stop() for adopted container, got: {:?}",
            *recorded,
        );
        assert!(
            !recorded.iter().any(|c| matches!(c, MockCall::Remove { .. })),
            "Should NOT call provider.remove() for adopted container, got: {:?}",
            *recorded,
        );
    }

    // ==================== Discovery: forget ====================

    #[tokio::test]
    async fn test_forget_removes_from_state() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.forget(&id).await.unwrap();

        let found = mgr.get(&id).await.unwrap();
        assert!(found.is_none(), "forget should remove from state");
    }

    #[tokio::test]
    async fn test_forget_nonexistent_succeeds() {
        let mock = MockProvider::new(ProviderType::Docker);
        let mgr = test_manager(mock);
        // Forgetting a non-existent ID should not error
        mgr.forget("nonexistent-id").await.unwrap();
    }

    #[tokio::test]
    async fn test_discover_calls_provider() {
        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();
        let mgr = test_manager(mock);

        let result = mgr.discover().await.unwrap();
        assert!(result.is_empty()); // default mock returns empty vec

        let recorded = calls.lock().unwrap();
        assert!(recorded.iter().any(|c| matches!(c, MockCall::Discover)));
    }

    // ==================== Lifecycle: edge cases ====================

    #[tokio::test]
    async fn test_first_create_lifecycle_skips_absent_commands() {
        // Only postCreateCommand is set — verify only one lifecycle exec runs
        let workspace = create_test_workspace();
        std::fs::write(
            workspace.path().join(".devcontainer/devcontainer.json"),
            r#"{"image": "ubuntu:22.04", "postCreateCommand": "echo only-post-create"}"#,
        )
        .unwrap();

        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Built,
            Some("sha256:mock_image_id"),
            None,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.up(&id).await.unwrap();

        let recorded = calls.lock().unwrap();
        let execs = exec_commands(&recorded);
        // Should have exactly one lifecycle exec (postCreate) plus one for postStart (none configured → 0)
        let cmds: Vec<&str> = execs.iter().map(|cmd| shell_cmd(cmd)).collect();
        assert!(
            cmds.contains(&"echo only-post-create"),
            "postCreateCommand should run; got {:?}",
            cmds
        );
        assert!(!cmds.contains(&"echo on-create"), "onCreateCommand should NOT run when absent");
        assert!(!cmds.contains(&"echo update-content"), "updateContentCommand should NOT run when absent");
    }

    #[tokio::test]
    async fn test_post_attach_no_config_succeeds() {
        // devcontainer.json has no postAttachCommand — should succeed with no execs
        let workspace = create_test_workspace();

        let mock = MockProvider::new(ProviderType::Docker);
        let calls = mock.calls.clone();

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Running,
            Some("sha256:img"),
            Some("container123"),
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_no_creds(mock, state);
        mgr.run_post_attach_command(&id).await.unwrap();

        let recorded = calls.lock().unwrap();
        let execs = exec_commands(&recorded);
        assert!(execs.is_empty(), "No postAttachCommand → no execs; got {:?}", execs);
    }

    // ==================== Compose: edge cases ====================

    #[tokio::test]
    async fn test_up_compose_stores_metadata() {
        let workspace = create_compose_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        *mock.compose_ps_result.lock().unwrap() = Ok(vec![ComposeServiceInfo {
            service_name: "app".to_string(),
            container_id: ContainerId::new("compose_meta_abc"),
            status: ContainerStatus::Running,
        }]);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Configured,
            None,
            None,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        mgr.up(&id).await.unwrap();

        let cs = mgr.get(&id).await.unwrap().unwrap();
        assert_eq!(cs.container_id, Some("compose_meta_abc".to_string()));
        assert!(cs.compose_project.is_some(), "compose_project should be stored");
        assert_eq!(cs.compose_service, Some("app".to_string()));
        assert_eq!(cs.status, DevcContainerStatus::Running);
    }

    #[tokio::test]
    async fn test_up_compose_service_not_found_fails() {
        let workspace = create_compose_workspace();
        let mock = MockProvider::new(ProviderType::Docker);
        // compose_ps returns a service that does NOT match "app"
        *mock.compose_ps_result.lock().unwrap() = Ok(vec![ComposeServiceInfo {
            service_name: "wrong-service".to_string(),
            container_id: ContainerId::new("wrong_id"),
            status: ContainerStatus::Running,
        }]);

        let mut state = StateStore::new();
        let cs = make_container_state(
            workspace.path(),
            DevcContainerStatus::Configured,
            None,
            None,
        );
        let id = cs.id.clone();
        state.add(cs);

        let mgr = test_manager_with_state(mock, state);
        let result = mgr.up(&id).await;
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("not found"), "Expected 'not found' in: {}", err);
    }

    // ==================== MockProvider assertion helpers ====================

    #[tokio::test]
    async fn test_mock_exec_responses_queue() {
        let mock = MockProvider::new(ProviderType::Docker);
        mock.exec_responses.lock().unwrap().extend(vec![
            (0, "first".to_string()),
            (1, "second".to_string()),
        ]);

        let cid = ContainerId::new("test");
        let cfg = devc_provider::ExecConfig {
            cmd: vec!["echo".to_string()],
            env: HashMap::new(),
            working_dir: None,
            user: None,
            tty: false,
            stdin: false,
            privileged: false,
        };

        let r1 = mock.exec(&cid, &cfg).await.unwrap();
        assert_eq!(r1.output, "first");
        assert_eq!(r1.exit_code, 0);

        let r2 = mock.exec(&cid, &cfg).await.unwrap();
        assert_eq!(r2.output, "second");
        assert_eq!(r2.exit_code, 1);

        // Queue exhausted — falls back to default
        let r3 = mock.exec(&cid, &cfg).await.unwrap();
        assert_eq!(r3.exit_code, 0);
        assert!(r3.output.is_empty());
    }

    #[tokio::test]
    async fn test_mock_inspect_responses_queue() {
        let mock = MockProvider::new(ProviderType::Docker);
        mock.inspect_responses.lock().unwrap().push(
            Ok(mock_container_details("queued_id", ContainerStatus::Exited)),
        );

        let cid = ContainerId::new("test");
        let r1 = mock.inspect(&cid).await.unwrap();
        assert_eq!(r1.status, ContainerStatus::Exited);

        // Queue exhausted — falls back to default (Running)
        let r2 = mock.inspect(&cid).await.unwrap();
        assert_eq!(r2.status, ContainerStatus::Running);
    }

    #[test]
    fn test_mock_call_count() {
        let mock = MockProvider::new(ProviderType::Docker);
        mock.calls.lock().unwrap().extend(vec![
            MockCall::Exec { id: "a".into(), cmd: vec![], working_dir: None, user: None },
            MockCall::Start { id: "a".into() },
            MockCall::Exec { id: "b".into(), cmd: vec![], working_dir: None, user: None },
        ]);

        assert_eq!(mock.call_count(|c| matches!(c, MockCall::Exec { .. })), 2);
        assert_eq!(mock.call_count(|c| matches!(c, MockCall::Start { .. })), 1);
        assert_eq!(mock.call_count(|c| matches!(c, MockCall::Stop { .. })), 0);
    }

    #[test]
    fn test_mock_assert_call_order() {
        let mock = MockProvider::new(ProviderType::Docker);
        mock.calls.lock().unwrap().extend(vec![
            MockCall::Build { tag: "t".into() },
            MockCall::Create { image: "i".into(), name: None },
            MockCall::Start { id: "x".into() },
            MockCall::Exec { id: "x".into(), cmd: vec![], working_dir: None, user: None },
        ]);

        // This should pass — subsequence matches
        mock.assert_call_order(&["Build", "Create", "Start", "Exec"]);
        // Partial subsequence should also pass
        mock.assert_call_order(&["Build", "Exec"]);
    }

    #[test]
    fn test_mock_exec_commands_helper() {
        let mock = MockProvider::new(ProviderType::Docker);
        mock.calls.lock().unwrap().extend(vec![
            MockCall::Start { id: "a".into() },
            MockCall::Exec { id: "a".into(), cmd: vec!["echo".into(), "hello".into()], working_dir: None, user: None },
            MockCall::Exec { id: "b".into(), cmd: vec!["ls".into()], working_dir: None, user: None },
        ]);

        let cmds = mock.exec_commands();
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0], vec!["echo", "hello"]);
        assert_eq!(cmds[1], vec!["ls"]);
    }
}
