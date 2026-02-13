//! Container manager - coordinates all container operations

use crate::{
    run_feature_lifecycle_commands, run_lifecycle_command_with_env, Container, ContainerState,
    CoreError, DevcContainerStatus, DotfilesManager, EnhancedBuildContext, Result, SshManager,
    StateStore,
};
use devc_config::{GlobalConfig, ImageSource};
use devc_provider::{
    ContainerId, ContainerProvider, ContainerStatus, DiscoveredContainer, ExecStream, LogConfig,
    ProviderType,
};
use crate::features;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::RwLock;

/// Main container manager
pub struct ContainerManager {
    /// Container provider (None when disconnected)
    provider: Option<Box<dyn ContainerProvider>>,
    /// State store
    state: Arc<RwLock<StateStore>>,
    /// Global configuration
    global_config: GlobalConfig,
    /// Error message when disconnected
    connection_error: Option<String>,
}

/// Build progress callback
pub type BuildProgressCallback = Box<dyn Fn(&str) + Send + Sync>;

impl ContainerManager {
    /// Create a new container manager
    pub async fn new(provider: Box<dyn ContainerProvider>) -> Result<Self> {
        let global_config = GlobalConfig::load()?;
        let state = StateStore::load()?;

        Ok(Self {
            provider: Some(provider),
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

        Ok(Self {
            provider: Some(provider),
            state: Arc::new(RwLock::new(state)),
            global_config,
            connection_error: None,
        })
    }

    /// Create a manager for testing with injectable dependencies
    #[cfg(test)]
    pub fn new_for_testing(
        provider: Box<dyn ContainerProvider>,
        global_config: GlobalConfig,
        state: StateStore,
    ) -> Self {
        Self {
            provider: Some(provider),
            state: Arc::new(RwLock::new(state)),
            global_config,
            connection_error: None,
        }
    }

    /// Create a disconnected manager for testing
    #[cfg(test)]
    pub fn disconnected_for_testing(
        global_config: GlobalConfig,
        state: StateStore,
        error: String,
    ) -> Self {
        Self {
            provider: None,
            state: Arc::new(RwLock::new(state)),
            global_config,
            connection_error: Some(error),
        }
    }

    /// Create a disconnected manager (no provider available)
    pub fn disconnected(global_config: GlobalConfig, error: String) -> Result<Self> {
        let state = StateStore::load()?;

        Ok(Self {
            provider: None,
            state: Arc::new(RwLock::new(state)),
            global_config,
            connection_error: Some(error),
        })
    }

    /// Check if connected to a provider
    pub fn is_connected(&self) -> bool {
        self.provider.is_some()
    }

    /// Get the connection error message (if disconnected)
    pub fn connection_error(&self) -> Option<&str> {
        self.connection_error.as_deref()
    }

    /// Connect to a provider (for reconnection)
    pub fn connect(&mut self, provider: Box<dyn ContainerProvider>) {
        self.provider = Some(provider);
        self.connection_error = None;
    }

    /// Get the provider, returning an error if not connected
    fn require_provider(&self) -> Result<&dyn ContainerProvider> {
        self.provider.as_deref().ok_or_else(|| {
            CoreError::NotConnected(
                self.connection_error
                    .clone()
                    .unwrap_or_else(|| "No container provider available".to_string()),
            )
        })
    }

    /// Get the provider type (None if disconnected)
    pub fn provider_type(&self) -> Option<ProviderType> {
        self.provider.as_ref().map(|p| p.info().provider_type)
    }

    /// Get a reference to the container provider (for advanced operations like port detection)
    pub fn provider(&self) -> Option<&dyn ContainerProvider> {
        self.provider.as_deref()
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
        let provider = self.require_provider()?;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        if container_state.status != DevcContainerStatus::Running {
            return Ok(crate::credentials::CredentialStatus::default());
        }

        let container_id = container_state
            .container_id
            .as_ref()
            .ok_or_else(|| CoreError::InvalidState("Container has no container ID".to_string()))?;
        let cid = ContainerId::new(container_id);

        let user = Container::from_config(&container_state.config_path)
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
        state.save()?;

        Ok(container_state)
    }

    /// Initialize a new container from a specific config path.
    /// Returns Ok(None) if the config is already registered (not an error).
    pub async fn init_from_config(&self, config_path: &Path) -> Result<Option<ContainerState>> {
        let provider_type = self
            .provider_type()
            .ok_or_else(|| CoreError::NotConnected("Cannot init: no provider available".to_string()))?;

        let container = Container::from_config(config_path)?;

        let mut state = self.state.write().await;

        // Already registered — skip silently
        if state.find_by_config_path(&container.config_path).is_some() {
            return Ok(None);
        }

        let container_state = ContainerState::new(
            container.name.clone(),
            provider_type,
            container.config_path.clone(),
            container.workspace_path.clone(),
        );

        state.add(container_state.clone());
        state.save()?;

        Ok(Some(container_state))
    }

    /// Auto-discover all devcontainer.json configs in a workspace directory
    /// and register any that aren't already tracked.
    /// Returns the list of newly registered container states.
    pub async fn auto_discover_configs(&self, workspace_dir: &Path) -> Result<Vec<ContainerState>> {
        use devc_config::DevContainerConfig;

        let all_configs = DevContainerConfig::load_all_from_dir(workspace_dir);
        let mut newly_registered = Vec::new();

        for (_config, config_path) in all_configs {
            match self.init_from_config(&config_path).await {
                Ok(Some(cs)) => newly_registered.push(cs),
                Ok(None) => {} // already registered
                Err(e) => {
                    tracing::warn!(
                        "Skipping config {}: {}",
                        config_path.display(),
                        e
                    );
                }
            }
        }

        Ok(newly_registered)
    }

    /// Build a container image
    pub async fn build(&self, id: &str) -> Result<String> {
        self.build_with_options(id, false).await
    }

    /// Build a container image with options
    pub async fn build_with_options(&self, id: &str, no_cache: bool) -> Result<String> {
        let provider = self.require_provider()?;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        // Load container config
        let container = Container::from_config(&container_state.config_path)?;

        // Update status to building
        {
            let mut state = self.state.write().await;
            if let Some(cs) = state.get_mut(id) {
                cs.status = DevcContainerStatus::Building;
            }
            state.save()?;
        }

        // Check if SSH injection is enabled
        let inject_ssh = self.global_config.defaults.ssh_enabled.unwrap_or(false);

        // Resolve devcontainer features
        let config_dir = container.config_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let resolved_features = if let Some(ref feature_map) = container.devcontainer.features {
            features::resolve_and_prepare_features(feature_map, &config_dir, &None).await?
        } else {
            vec![]
        };
        let has_features = !resolved_features.is_empty();
        let feature_properties = features::merge_feature_properties(&resolved_features);
        let remote_user = container.devcontainer.effective_user().unwrap_or("root").to_string();

        // Check if we need to build or pull
        let image_id = match container.devcontainer.image_source() {
            ImageSource::Image(image) => {
                if has_features || inject_ssh {
                    // Need a build: features and/or SSH injection
                    tracing::info!(
                        "Building enhanced image from {} (features: {}, SSH: {})",
                        image, has_features, inject_ssh
                    );
                    let enhanced_ctx = if has_features {
                        EnhancedBuildContext::from_image_with_features(
                            &image, &resolved_features, inject_ssh, &remote_user,
                        )?
                    } else {
                        EnhancedBuildContext::from_image(&image)?
                    };

                    let build_config = devc_provider::BuildConfig {
                        context: enhanced_ctx.context_path().to_path_buf(),
                        dockerfile: enhanced_ctx.dockerfile_name().to_string(),
                        tag: container.image_tag(),
                        build_args: std::collections::HashMap::new(),
                        target: None,
                        cache_from: Vec::new(),
                        labels: {
                            let mut labels = std::collections::HashMap::new();
                            labels.insert("devc.managed".to_string(), "true".to_string());
                            labels.insert("devc.project".to_string(), container.name.clone());
                            labels.insert("devc.base_image".to_string(), image.clone());
                            labels
                        },
                        no_cache,
                        pull: true,
                    };

                    let result = provider.build(&build_config).await;
                    match result {
                        Ok(id) => id.0,
                        Err(e) => {
                            self.set_status(id, DevcContainerStatus::Failed).await?;
                            return Err(e.into());
                        }
                    }
                } else {
                    // Just pull the image directly (no features, no SSH)
                    tracing::info!("Pulling image: {}", image);
                    let result = provider.pull(&image).await;
                    match result {
                        Ok(id) => id.0,
                        Err(e) => {
                            self.set_status(id, DevcContainerStatus::Failed).await?;
                            return Err(e.into());
                        }
                    }
                }
            }
            ImageSource::Dockerfile { .. } => {
                let mut build_config = container.build_config()?;
                build_config.no_cache = no_cache;

                if has_features || inject_ssh {
                    tracing::info!(
                        "Building enhanced image: {} (features: {}, SSH: {}, no_cache: {})",
                        build_config.tag, has_features, inject_ssh, no_cache
                    );
                    let enhanced_ctx = if has_features {
                        EnhancedBuildContext::from_dockerfile_with_features(
                            &build_config.context,
                            &build_config.dockerfile,
                            &resolved_features,
                            inject_ssh,
                            &remote_user,
                        )?
                    } else {
                        EnhancedBuildContext::from_dockerfile(
                            &build_config.context,
                            &build_config.dockerfile,
                        )?
                    };

                    build_config.context = enhanced_ctx.context_path().to_path_buf();
                    build_config.dockerfile = enhanced_ctx.dockerfile_name().to_string();

                    let result = provider.build(&build_config).await;
                    match result {
                        Ok(id) => id.0,
                        Err(e) => {
                            self.set_status(id, DevcContainerStatus::Failed).await?;
                            return Err(e.into());
                        }
                    }
                } else {
                    tracing::info!(
                        "Building image: {} (no_cache: {})",
                        build_config.tag,
                        no_cache
                    );

                    let result = provider.build(&build_config).await;
                    match result {
                        Ok(id) => id.0,
                        Err(e) => {
                            self.set_status(id, DevcContainerStatus::Failed).await?;
                            return Err(e.into());
                        }
                    }
                }
            }
            ImageSource::Compose => {
                // Compose builds happen during `compose up`, mark as built
                tracing::info!("Compose project: skipping standalone build");
                {
                    let mut state = self.state.write().await;
                    if let Some(cs) = state.get_mut(id) {
                        cs.image_id = Some("compose".to_string());
                        cs.status = DevcContainerStatus::Built;
                        if let Ok(props_json) = serde_json::to_string(&feature_properties) {
                            cs.metadata.insert("feature_properties".to_string(), props_json);
                        }
                    }
                    state.save()?;
                }
                return Ok("compose".to_string());
            }
            ImageSource::None => {
                self.set_status(id, DevcContainerStatus::Failed).await?;
                return Err(CoreError::InvalidState(
                    "No image source specified".to_string(),
                ));
            }
        };

        // Update state with image ID and feature properties
        {
            let mut state = self.state.write().await;
            if let Some(cs) = state.get_mut(id) {
                cs.image_id = Some(image_id.clone());
                cs.status = DevcContainerStatus::Built;
                if let Ok(props_json) = serde_json::to_string(&feature_properties) {
                    cs.metadata.insert("feature_properties".to_string(), props_json);
                }
            }
            state.save()?;
        }

        Ok(image_id)
    }

    /// Create a container from a built image
    pub async fn create(&self, id: &str) -> Result<ContainerId> {
        let provider = self.require_provider()?;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        let image_id = container_state.image_id.as_ref().ok_or_else(|| {
            CoreError::InvalidState("Container image not built yet".to_string())
        })?;

        let container = Container::from_config(&container_state.config_path)?;

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
            state.save()?;
        }

        Ok(container_id)
    }

    /// Start a container
    pub async fn start(&self, id: &str) -> Result<()> {
        let provider = self.require_provider()?;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

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
            || Container::from_config(&container_state.config_path)
                .map(|c| c.is_compose())
                .unwrap_or(false);
        if is_compose {
            let container = Container::from_config(&container_state.config_path)?;
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
                    self.ensure_ssh_daemon_running(&svc.container_id).await?;
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
            self.ensure_ssh_daemon_running(&ContainerId::new(container_id)).await?;
        }

        // Run post-start commands (feature commands first, then devcontainer.json)
        let container = Container::from_config(&container_state.config_path)?;
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
    async fn ensure_ssh_daemon_running(&self, container_id: &ContainerId) -> Result<()> {
        let provider = self.require_provider()?;

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
        let provider = self.require_provider()?;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        if !container_state.can_stop() {
            return Err(CoreError::InvalidState(format!(
                "Container cannot be stopped in {} state",
                container_state.status
            )));
        }

        // Handle compose stop: bring down all services
        if let Some(ref compose_project) = container_state.compose_project {
            let container = Container::from_config(&container_state.config_path)?;
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

        // Remove the container if it exists (only if we have a provider)
        if let Some(ref container_id) = container_state.container_id {
            if let Some(ref provider) = self.provider {
                if let Err(e) = provider
                    .remove(&ContainerId::new(container_id), force)
                    .await
                {
                    tracing::warn!("Failed to remove container {}: {}", container_id, e);
                }
            }
        }

        // Remove from state
        {
            let mut state = self.state.write().await;
            state.remove(id);
            state.save()?;
        }

        Ok(())
    }

    /// Stop and remove the runtime container, but keep the state so it can be recreated with `up`
    pub async fn down(&self, id: &str) -> Result<()> {
        let provider = self.require_provider()?;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        // Handle compose teardown
        if let Some(ref compose_project) = container_state.compose_project {
            let container = Container::from_config(&container_state.config_path)?;
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
            state.save()?;
        }

        Ok(())
    }

    /// Rebuild a container, optionally migrating to current provider
    ///
    /// This will:
    /// 1. Stop and remove the runtime container (via down())
    /// 2. If provider changed: update state with new provider, clear image_id
    /// 3. Build image with optional --no-cache
    /// 4. Create and start the new container
    pub async fn rebuild(&self, id: &str, no_cache: bool) -> Result<()> {
        let new_provider = self
            .provider_type()
            .ok_or_else(|| CoreError::NotConnected("Cannot rebuild: no provider available".to_string()))?;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        let old_provider = container_state.provider;
        let provider_changed = old_provider != new_provider;

        // 1. Stop and remove runtime container
        if container_state.container_id.is_some() {
            self.down(id).await?;
        }

        // 2. Handle provider migration
        if provider_changed {
            // Update state with new provider
            // Image is provider-specific, so clear it to force rebuild
            {
                let mut state = self.state.write().await;
                if let Some(cs) = state.get_mut(id) {
                    cs.provider = new_provider;
                    cs.image_id = None;
                    cs.container_id = None;
                    cs.status = DevcContainerStatus::Configured;
                }
                state.save()?;
            }
            tracing::info!(
                "Provider migration: {} -> {}",
                old_provider,
                new_provider
            );
        }

        // 3. Rebuild image
        self.build_with_options(id, no_cache).await?;

        // 4. Create and start container
        self.up(id).await?;

        Ok(())
    }

    /// Rebuild a container with progress updates streamed to a channel
    ///
    /// Same as rebuild() but sends progress updates for TUI display
    pub async fn rebuild_with_progress(
        &self,
        id: &str,
        no_cache: bool,
        progress: mpsc::UnboundedSender<String>,
    ) -> Result<()> {
        let new_provider = self
            .provider_type()
            .ok_or_else(|| CoreError::NotConnected("Cannot rebuild: no provider available".to_string()))?;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        let old_provider = container_state.provider;
        let provider_changed = old_provider != new_provider;

        // 1. Stop and remove runtime container
        if container_state.container_id.is_some() {
            let _ = progress.send("Stopping container...".to_string());
            self.down(id).await?;
        }

        // 2. Handle provider migration
        if provider_changed {
            let _ = progress.send(format!(
                "Migrating provider: {} -> {}",
                old_provider, new_provider
            ));
            let mut state = self.state.write().await;
            if let Some(cs) = state.get_mut(id) {
                cs.provider = new_provider;
                cs.image_id = None;
                cs.container_id = None;
                cs.status = DevcContainerStatus::Configured;
            }
            state.save()?;
        }

        // 3. Rebuild image with progress
        self.build_with_progress(id, no_cache, progress.clone()).await?;

        // 4. Create and start container with progress (runs lifecycle commands, dotfiles, SSH setup)
        self.up_with_progress(id, Some(&progress), None).await?;

        let _ = progress.send("Rebuild complete!".to_string());
        Ok(())
    }

    /// Build a container image with progress updates streamed to a channel
    pub async fn build_with_progress(
        &self,
        id: &str,
        no_cache: bool,
        progress: mpsc::UnboundedSender<String>,
    ) -> Result<String> {
        let provider = self.require_provider()?;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        // Load container config
        let container = Container::from_config(&container_state.config_path)?;

        // Update status to building
        {
            let mut state = self.state.write().await;
            if let Some(cs) = state.get_mut(id) {
                cs.status = DevcContainerStatus::Building;
            }
            state.save()?;
        }

        // Check if SSH injection is enabled
        let inject_ssh = self.global_config.defaults.ssh_enabled.unwrap_or(false);

        // Log SSH injection status (visible without -v flag)
        if inject_ssh {
            let _ = progress.send("SSH support: Injecting dropbear into image...".to_string());
        } else {
            let _ = progress.send("SSH support: Disabled (not injecting dropbear)".to_string());
        }

        // Resolve devcontainer features
        let config_dir = container.config_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let progress_opt = Some(progress.clone());
        let resolved_features = if let Some(ref feature_map) = container.devcontainer.features {
            features::resolve_and_prepare_features(feature_map, &config_dir, &progress_opt).await?
        } else {
            vec![]
        };
        let has_features = !resolved_features.is_empty();
        let feature_properties = features::merge_feature_properties(&resolved_features);
        let remote_user = container.devcontainer.effective_user().unwrap_or("root").to_string();

        if has_features {
            let _ = progress.send(format!("Installing {} devcontainer feature(s)...", resolved_features.len()));
        }

        // Check if we need to build or pull
        let image_id = match container.devcontainer.image_source() {
            ImageSource::Image(image) => {
                if has_features || inject_ssh {
                    let _ = progress.send(format!(
                        "Building enhanced image from {} (features: {}, SSH: {})",
                        image, has_features, inject_ssh
                    ));
                    let enhanced_ctx = if has_features {
                        EnhancedBuildContext::from_image_with_features(
                            &image, &resolved_features, inject_ssh, &remote_user,
                        )?
                    } else {
                        EnhancedBuildContext::from_image(&image)?
                    };

                    let build_config = devc_provider::BuildConfig {
                        context: enhanced_ctx.context_path().to_path_buf(),
                        dockerfile: enhanced_ctx.dockerfile_name().to_string(),
                        tag: container.image_tag(),
                        build_args: std::collections::HashMap::new(),
                        target: None,
                        cache_from: Vec::new(),
                        labels: {
                            let mut labels = std::collections::HashMap::new();
                            labels.insert("devc.managed".to_string(), "true".to_string());
                            labels.insert("devc.project".to_string(), container.name.clone());
                            labels.insert("devc.base_image".to_string(), image.clone());
                            labels
                        },
                        no_cache,
                        pull: true,
                    };

                    let result = provider
                        .build_with_progress(&build_config, progress.clone())
                        .await;
                    match result {
                        Ok(id) => id.0,
                        Err(e) => {
                            self.set_status(id, DevcContainerStatus::Failed).await?;
                            return Err(e.into());
                        }
                    }
                } else {
                    let _ = progress.send(format!("Pulling image: {}", image));
                    let result = provider.pull(&image).await;
                    match result {
                        Ok(id) => id.0,
                        Err(e) => {
                            self.set_status(id, DevcContainerStatus::Failed).await?;
                            return Err(e.into());
                        }
                    }
                }
            }
            ImageSource::Dockerfile { .. } => {
                let mut build_config = container.build_config()?;
                build_config.no_cache = no_cache;

                if has_features || inject_ssh {
                    let _ = progress.send(format!(
                        "Building enhanced image: {} (features: {}, SSH: {}, no_cache: {})",
                        build_config.tag, has_features, inject_ssh, no_cache
                    ));
                    let enhanced_ctx = if has_features {
                        EnhancedBuildContext::from_dockerfile_with_features(
                            &build_config.context,
                            &build_config.dockerfile,
                            &resolved_features,
                            inject_ssh,
                            &remote_user,
                        )?
                    } else {
                        EnhancedBuildContext::from_dockerfile(
                            &build_config.context,
                            &build_config.dockerfile,
                        )?
                    };

                    build_config.context = enhanced_ctx.context_path().to_path_buf();
                    build_config.dockerfile = enhanced_ctx.dockerfile_name().to_string();

                    let result = provider
                        .build_with_progress(&build_config, progress.clone())
                        .await;
                    match result {
                        Ok(id) => id.0,
                        Err(e) => {
                            self.set_status(id, DevcContainerStatus::Failed).await?;
                            return Err(e.into());
                        }
                    }
                } else {
                    let _ = progress.send(format!(
                        "Building image: {} (no_cache: {})",
                        build_config.tag, no_cache
                    ));

                    let result = provider
                        .build_with_progress(&build_config, progress.clone())
                        .await;
                    match result {
                        Ok(id) => id.0,
                        Err(e) => {
                            self.set_status(id, DevcContainerStatus::Failed).await?;
                            return Err(e.into());
                        }
                    }
                }
            }
            ImageSource::Compose => {
                // Compose builds happen during `compose up`, mark as built
                let _ = progress.send("Compose project: build will happen during 'up'".to_string());
                {
                    let mut state = self.state.write().await;
                    if let Some(cs) = state.get_mut(id) {
                        cs.image_id = Some("compose".to_string());
                        cs.status = DevcContainerStatus::Built;
                        if let Ok(props_json) = serde_json::to_string(&feature_properties) {
                            cs.metadata.insert("feature_properties".to_string(), props_json);
                        }
                    }
                    state.save()?;
                }
                return Ok("compose".to_string());
            }
            ImageSource::None => {
                self.set_status(id, DevcContainerStatus::Failed).await?;
                return Err(CoreError::InvalidState(
                    "No image source specified".to_string(),
                ));
            }
        };

        // Update state with image ID and feature properties
        {
            let mut state = self.state.write().await;
            if let Some(cs) = state.get_mut(id) {
                cs.image_id = Some(image_id.clone());
                cs.status = DevcContainerStatus::Built;
                if let Ok(props_json) = serde_json::to_string(&feature_properties) {
                    cs.metadata.insert("feature_properties".to_string(), props_json);
                }
            }
            state.save()?;
        }

        Ok(image_id)
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
        let provider = self.require_provider()?;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        // Run initializeCommand on host before build
        let container = Container::from_config(&container_state.config_path)?;
        {
            if let Some(ref cmd) = container.devcontainer.initialize_command {
                send_progress(progress, "Running initializeCommand on host...");
                crate::run_host_command(cmd, &container.workspace_path, output).await?;
            }
            if let Some(ref wait_for) = container.devcontainer.wait_for {
                tracing::info!("waitFor is set to '{}' (async lifecycle deferral not yet implemented)", wait_for);
            }
        }

        // Handle Docker Compose projects
        if container.is_compose() {
            return self
                .up_compose(id, &container, provider, progress)
                .await;
        }

        // Build if needed
        if container_state.image_id.is_none() {
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

        // Run onCreate command if this is first create
        if container_state.status == DevcContainerStatus::Created {
            let feature_props = get_feature_properties(&container_state);
            let user = container.devcontainer.effective_user();
            let workspace_folder = container.devcontainer.workspace_folder.as_deref();
            let merged_env = merge_remote_env(
                container.devcontainer.remote_env.as_ref(),
                &feature_props.remote_env,
            );
            let remote_env = merged_env.as_ref();

            // Feature onCreateCommands run first (per spec)
            if !feature_props.on_create_commands.is_empty() {
                send_progress(progress, "Running feature onCreateCommand(s)...");
                provider.start(&container_id).await?;
                run_feature_lifecycle_commands(
                    provider, &container_id, &feature_props.on_create_commands,
                    user, workspace_folder, remote_env,
                ).await?;
            }

            if let Some(ref cmd) = container.devcontainer.on_create_command {
                send_progress(progress, "Running onCreate command...");
                // Start the container first for onCreate
                let details = provider.inspect(&container_id).await?;
                if details.status != ContainerStatus::Running {
                    provider.start(&container_id).await?;
                }

                run_lifecycle_command_with_env(
                    provider, &container_id, cmd, user, workspace_folder, remote_env,
                ).await?;
            }

            // Feature updateContentCommands run first (per spec)
            if !feature_props.update_content_commands.is_empty() {
                send_progress(progress, "Running feature updateContentCommand(s)...");
                let details = provider.inspect(&container_id).await?;
                if details.status != ContainerStatus::Running {
                    provider.start(&container_id).await?;
                }
                run_feature_lifecycle_commands(
                    provider, &container_id, &feature_props.update_content_commands,
                    user, workspace_folder, remote_env,
                ).await?;
            }

            // Run updateContentCommand (between onCreate and postCreate per spec)
            if let Some(ref cmd) = container.devcontainer.update_content_command {
                send_progress(progress, "Running updateContentCommand...");
                let details = provider.inspect(&container_id).await?;
                if details.status != ContainerStatus::Running {
                    provider.start(&container_id).await?;
                }

                run_lifecycle_command_with_env(
                    provider, &container_id, cmd, user, workspace_folder, remote_env,
                ).await?;
            }

            // Feature postCreateCommands run first (per spec)
            if !feature_props.post_create_commands.is_empty() {
                send_progress(progress, "Running feature postCreateCommand(s)...");
                let details = provider.inspect(&container_id).await?;
                if details.status != ContainerStatus::Running {
                    provider.start(&container_id).await?;
                }
                run_feature_lifecycle_commands(
                    provider, &container_id, &feature_props.post_create_commands,
                    user, workspace_folder, remote_env,
                ).await?;
            }

            // Run postCreateCommand
            if let Some(ref cmd) = container.devcontainer.post_create_command {
                send_progress(progress, "Running postCreateCommand...");
                // Ensure container is started
                let details = provider.inspect(&container_id).await?;
                if details.status != ContainerStatus::Running {
                    provider.start(&container_id).await?;
                }

                run_lifecycle_command_with_env(
                    provider, &container_id, cmd, user, workspace_folder, remote_env,
                ).await?;
            }

            // Setup SSH if enabled (for proper TTY/resize support)
            if self.global_config.defaults.ssh_enabled.unwrap_or(false) {
                send_progress(progress, "Setting up SSH...");
                // Ensure container is running for SSH setup
                let details = provider.inspect(&container_id).await?;
                if details.status != ContainerStatus::Running {
                    provider.start(&container_id).await?;
                }

                let ssh_manager = SshManager::new()?;
                ssh_manager.ensure_keys_exist()?;

                let user = container.devcontainer.effective_user();
                match ssh_manager
                    .setup_container(provider, &container_id, user)
                    .await
                {
                    Ok(()) => {
                        tracing::info!("SSH setup completed for container");
                        let mut state = self.state.write().await;
                        if let Some(cs) = state.get_mut(id) {
                            cs.metadata
                                .insert("ssh_available".to_string(), "true".to_string());
                            if let Some(u) = user {
                                cs.metadata
                                    .insert("remote_user".to_string(), u.to_string());
                            }
                        }
                        state.save()?;
                    }
                    Err(e) => {
                        tracing::warn!("SSH setup failed (will use exec fallback): {}", e);
                        let mut state = self.state.write().await;
                        if let Some(cs) = state.get_mut(id) {
                            cs.metadata
                                .insert("ssh_available".to_string(), "false".to_string());
                        }
                        state.save()?;
                    }
                }
            }

            // Inject dotfiles
            let dotfiles_manager = if let Some(ref dotfiles_config) = container.devcontainer.dotfiles
            {
                DotfilesManager::from_devcontainer_config(dotfiles_config, &self.global_config)
            } else {
                DotfilesManager::from_global_config(&self.global_config)
            };

            if dotfiles_manager.is_configured() {
                send_progress(progress, "Installing dotfiles...");
                dotfiles_manager
                    .inject_with_progress(
                        provider,
                        &container_id,
                        container.devcontainer.effective_user(),
                        progress,
                    )
                    .await?;
            }
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

    /// Handle Docker Compose `up` flow
    ///
    /// 1. Run `compose up -d --build` to start all services
    /// 2. Find the dev service container ID via `compose ps`
    /// 3. Store compose metadata in state
    /// 4. Run lifecycle commands targeting the dev service container
    async fn up_compose(
        &self,
        id: &str,
        container: &Container,
        provider: &dyn ContainerProvider,
        progress: Option<&mpsc::UnboundedSender<String>>,
    ) -> Result<()> {
        let compose_files = container.compose_files().ok_or_else(|| {
            CoreError::InvalidState("No dockerComposeFile specified".to_string())
        })?;
        let service_name = container.compose_service().ok_or_else(|| {
            CoreError::InvalidState("No service specified for compose project".to_string())
        })?;
        let project_name = container.compose_project_name();

        let mut owned = compose_file_strs(&compose_files);

        // Resolve devcontainer features for compose override + exec-based install
        let config_dir = container.config_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let progress_opt: Option<mpsc::UnboundedSender<String>> = progress.map(|p| p.clone());
        let resolved_features = if let Some(ref feature_map) = container.devcontainer.features {
            features::resolve_and_prepare_features(feature_map, &config_dir, &progress_opt).await?
        } else {
            vec![]
        };
        let feature_props = features::merge_feature_properties(&resolved_features);

        // Generate compose override file if features declare container properties
        let override_file = if feature_props.has_container_properties() {
            let yaml = features::compose_override::generate_compose_override(
                service_name, &feature_props,
            );
            if let Some(yaml) = yaml {
                let path = container.workspace_path.join(".devc-compose-override.yml");
                std::fs::write(&path, &yaml)?;
                Some(path)
            } else {
                None
            }
        } else {
            None
        };

        // Add override file to compose files list
        if let Some(ref override_path) = override_file {
            owned.push(override_path.to_string_lossy().to_string());
        }

        let compose_file_refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();

        // 1. Run compose up
        send_progress(progress, "Running docker compose up...");
        let progress_tx: Option<mpsc::UnboundedSender<String>> = progress.map(|p| {
            let p = p.clone();
            let (real_tx, mut real_rx) = mpsc::unbounded_channel::<String>();
            tokio::spawn(async move {
                while let Some(msg) = real_rx.recv().await {
                    let _ = p.send(msg);
                }
            });
            real_tx
        });

        provider
            .compose_up(
                &compose_file_refs,
                &project_name,
                &container.workspace_path,
                progress_tx,
            )
            .await?;

        // Clean up override file
        if let Some(ref path) = override_file {
            let _ = std::fs::remove_file(path);
        }

        // 2. Find the dev service container ID
        send_progress(progress, "Finding service container...");
        // Use original compose files (without override) for ps
        let original_owned = compose_file_strs(&compose_files);
        let original_refs: Vec<&str> = original_owned.iter().map(|s| s.as_str()).collect();
        let services = provider
            .compose_ps(&original_refs, &project_name, &container.workspace_path)
            .await?;

        let dev_service = services
            .iter()
            .find(|s| s.service_name == service_name)
            .ok_or_else(|| {
                CoreError::InvalidState(format!(
                    "Service '{}' not found in compose project",
                    service_name
                ))
            })?;

        let container_id = dev_service.container_id.clone();

        // 3. Install features via exec if any were resolved
        if !resolved_features.is_empty() {
            send_progress(progress, "Installing features...");
            let remote_user = container.devcontainer.effective_user().unwrap_or("root");
            features::install::install_features_via_exec(
                provider, &container_id, &resolved_features, remote_user, progress,
            ).await?;
        }

        // 4. Store compose metadata in state
        {
            let mut state = self.state.write().await;
            if let Some(cs) = state.get_mut(id) {
                cs.container_id = Some(container_id.0.clone());
                cs.image_id = Some("compose".to_string());
                cs.compose_project = Some(project_name.clone());
                cs.compose_service = Some(service_name.to_string());
                cs.status = DevcContainerStatus::Running;
            }
            state.save()?;
        }

        // 5. Run lifecycle commands targeting the dev service container
        //    Feature lifecycle commands run BEFORE devcontainer.json commands (per spec)
        let user = container.devcontainer.effective_user();
        let workspace_folder = container.devcontainer.workspace_folder.as_deref();
        let merged_env = merge_remote_env(
            container.devcontainer.remote_env.as_ref(),
            &feature_props.remote_env,
        );
        let remote_env = merged_env.as_ref();

        if !feature_props.on_create_commands.is_empty() {
            send_progress(progress, "Running feature onCreateCommand(s)...");
            run_feature_lifecycle_commands(
                provider, &container_id, &feature_props.on_create_commands,
                user, workspace_folder, remote_env,
            ).await?;
        }

        if let Some(ref cmd) = container.devcontainer.on_create_command {
            send_progress(progress, "Running onCreate command...");
            run_lifecycle_command_with_env(
                provider, &container_id, cmd, user, workspace_folder, remote_env,
            ).await?;
        }

        if !feature_props.update_content_commands.is_empty() {
            send_progress(progress, "Running feature updateContentCommand(s)...");
            run_feature_lifecycle_commands(
                provider, &container_id, &feature_props.update_content_commands,
                user, workspace_folder, remote_env,
            ).await?;
        }

        if let Some(ref cmd) = container.devcontainer.update_content_command {
            send_progress(progress, "Running updateContentCommand...");
            run_lifecycle_command_with_env(
                provider, &container_id, cmd, user, workspace_folder, remote_env,
            ).await?;
        }

        if !feature_props.post_create_commands.is_empty() {
            send_progress(progress, "Running feature postCreateCommand(s)...");
            run_feature_lifecycle_commands(
                provider, &container_id, &feature_props.post_create_commands,
                user, workspace_folder, remote_env,
            ).await?;
        }

        if let Some(ref cmd) = container.devcontainer.post_create_command {
            send_progress(progress, "Running postCreateCommand...");
            run_lifecycle_command_with_env(
                provider, &container_id, cmd, user, workspace_folder, remote_env,
            ).await?;
        }

        // Setup SSH if enabled
        if self.global_config.defaults.ssh_enabled.unwrap_or(false) {
            send_progress(progress, "Setting up SSH...");
            let ssh_manager = SshManager::new()?;
            ssh_manager.ensure_keys_exist()?;

            match ssh_manager
                .setup_container(provider, &container_id, user)
                .await
            {
                Ok(()) => {
                    tracing::info!("SSH setup completed for compose container");
                    let mut state = self.state.write().await;
                    if let Some(cs) = state.get_mut(id) {
                        cs.metadata
                            .insert("ssh_available".to_string(), "true".to_string());
                        if let Some(u) = user {
                            cs.metadata
                                .insert("remote_user".to_string(), u.to_string());
                        }
                    }
                    state.save()?;
                }
                Err(e) => {
                    tracing::warn!("SSH setup failed (will use exec fallback): {}", e);
                    let mut state = self.state.write().await;
                    if let Some(cs) = state.get_mut(id) {
                        cs.metadata
                            .insert("ssh_available".to_string(), "false".to_string());
                    }
                    state.save()?;
                }
            }
        }

        if !feature_props.post_start_commands.is_empty() {
            send_progress(progress, "Running feature postStartCommand(s)...");
            run_feature_lifecycle_commands(
                provider, &container_id, &feature_props.post_start_commands,
                user, workspace_folder, remote_env,
            ).await?;
        }

        if let Some(ref cmd) = container.devcontainer.post_start_command {
            send_progress(progress, "Running postStartCommand...");
            run_lifecycle_command_with_env(
                provider, &container_id, cmd, user, workspace_folder, remote_env,
            ).await?;
        }

        send_progress(progress, "Compose project started!");
        Ok(())
    }

    /// Run postAttachCommand for a container (if configured)
    pub async fn run_post_attach_command(&self, id: &str) -> Result<()> {
        let provider = self.require_provider()?;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        let container = Container::from_config(&container_state.config_path)?;
        let container_id_str = container_state.container_id.as_ref().ok_or_else(|| {
            CoreError::InvalidState("Container not created yet".to_string())
        })?;
        let cid = ContainerId::new(container_id_str);

        // Feature postAttachCommands run first (per spec)
        let feature_props = get_feature_properties(&container_state);
        let merged_env = merge_remote_env(
            container.devcontainer.remote_env.as_ref(),
            &feature_props.remote_env,
        );
        if !feature_props.post_attach_commands.is_empty() {
            run_feature_lifecycle_commands(
                provider,
                &cid,
                &feature_props.post_attach_commands,
                container.devcontainer.effective_user(),
                container.devcontainer.workspace_folder.as_deref(),
                merged_env.as_ref(),
            )
            .await?;
        }

        if let Some(ref cmd) = container.devcontainer.post_attach_command {
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

    /// Execute a command in a container
    pub async fn exec(&self, id: &str, cmd: Vec<String>, tty: bool) -> Result<i64> {
        let result = self.exec_inner(id, cmd, tty).await?;
        Ok(result.exit_code)
    }

    /// Shared exec implementation
    async fn exec_inner(&self, id: &str, cmd: Vec<String>, tty: bool) -> Result<devc_provider::ExecResult> {
        let provider = self.require_provider()?;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        if container_state.status != DevcContainerStatus::Running {
            return Err(CoreError::InvalidState(
                "Container is not running".to_string(),
            ));
        }

        let container_id = container_state.container_id.as_ref().unwrap();
        let cid = ContainerId::new(container_id);

        // Try loading config for remoteEnv/user/workdir; fall back to a basic config
        // if the devcontainer.json is no longer accessible (e.g. tmp dir cleaned up)
        let feature_props = get_feature_properties(&container_state);
        let (config, user_for_creds) = match Container::from_config(&container_state.config_path) {
            Ok(container) => {
                let user = container.devcontainer.effective_user().map(|s| s.to_string());
                let feat_env = if feature_props.remote_env.is_empty() {
                    None
                } else {
                    Some(&feature_props.remote_env)
                };
                (container.exec_config_with_feature_env(cmd, tty, tty, feat_env), user)
            }
            Err(_) => {
                let mut env = std::collections::HashMap::new();
                env.insert("TERM".to_string(), "xterm-256color".to_string());
                env.insert("COLORTERM".to_string(), "truecolor".to_string());
                env.insert("LANG".to_string(), "C.UTF-8".to_string());
                env.insert("LC_ALL".to_string(), "C.UTF-8".to_string());
                (devc_provider::ExecConfig {
                    cmd,
                    env,
                    working_dir: None,
                    user: None,
                    tty,
                    stdin: tty,
                    privileged: false,
                }, None)
            }
        };

        // Refresh credential forwarding
        match crate::credentials::setup_credentials(
            provider,
            &cid,
            &self.global_config,
            user_for_creds.as_deref(),
            &container_state.workspace_path,
        )
        .await
        {
            Ok(status) if status.docker_registries > 0 || status.git_hosts > 0 => {
                tracing::info!(
                    "Credential forwarding: {} Docker registries, {} Git hosts",
                    status.docker_registries,
                    status.git_hosts
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("Credential forwarding setup failed (non-fatal): {}", e),
        }

        let result = provider
            .exec(&cid, &config)
            .await?;

        // Update last used
        {
            let mut state = self.state.write().await;
            state.touch(id);
            state.save()?;
        }

        Ok(result)
    }

    /// Execute a command interactively with PTY
    pub async fn exec_interactive(&self, id: &str, cmd: Vec<String>) -> Result<ExecStream> {
        let provider = self.require_provider()?;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        if container_state.status != DevcContainerStatus::Running {
            return Err(CoreError::InvalidState(
                "Container is not running".to_string(),
            ));
        }

        let container_id = container_state.container_id.as_ref().unwrap();
        let cid = ContainerId::new(container_id);
        let container = Container::from_config(&container_state.config_path)?;
        let feature_props = get_feature_properties(&container_state);
        let feat_env = if feature_props.remote_env.is_empty() {
            None
        } else {
            Some(&feature_props.remote_env)
        };

        // Refresh credential forwarding
        match crate::credentials::setup_credentials(
            provider,
            &cid,
            &self.global_config,
            container.devcontainer.effective_user(),
            &container_state.workspace_path,
        )
        .await
        {
            Ok(status) if status.docker_registries > 0 || status.git_hosts > 0 => {
                tracing::info!(
                    "Credential forwarding: {} Docker registries, {} Git hosts",
                    status.docker_registries,
                    status.git_hosts
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("Credential forwarding setup failed (non-fatal): {}", e),
        }

        let config = container.exec_config_with_feature_env(cmd, true, true, feat_env);
        let stream = provider
            .exec_interactive(&cid, &config)
            .await?;

        // Update last used
        {
            let mut state = self.state.write().await;
            state.touch(id);
            state.save()?;
        }

        Ok(stream)
    }

    /// Open an interactive shell in a container
    pub async fn shell(&self, id: &str) -> Result<ExecStream> {
        let provider = self.require_provider()?;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        if container_state.status != DevcContainerStatus::Running {
            return Err(CoreError::InvalidState(
                "Container is not running".to_string(),
            ));
        }

        let container_id = container_state.container_id.as_ref().unwrap();
        let cid = ContainerId::new(container_id);
        let container = Container::from_config(&container_state.config_path)?;
        let feature_props = get_feature_properties(&container_state);
        let feat_env = if feature_props.remote_env.is_empty() {
            None
        } else {
            Some(&feature_props.remote_env)
        };

        // Refresh credential forwarding (inject helpers on first call, refresh cache every time)
        match crate::credentials::setup_credentials(
            provider,
            &cid,
            &self.global_config,
            container.devcontainer.effective_user(),
            &container_state.workspace_path,
        )
        .await
        {
            Ok(status) if status.docker_registries > 0 || status.git_hosts > 0 => {
                tracing::info!(
                    "Credential forwarding: {} Docker registries, {} Git hosts",
                    status.docker_registries,
                    status.git_hosts
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("Credential forwarding setup failed (non-fatal): {}", e),
        }

        let config = container.shell_config_with_feature_env(feat_env);
        let stream = provider
            .exec_interactive(&cid, &config)
            .await?;

        // Update last used
        {
            let mut state = self.state.write().await;
            state.touch(id);
            state.save()?;
        }

        Ok(stream)
    }

    /// Sync container status with actual provider status
    ///
    /// If not connected to a provider, returns the current status without syncing.
    pub async fn sync_status(&self, id: &str) -> Result<DevcContainerStatus> {
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        // If no provider, just return current status
        let provider = match self.provider.as_ref() {
            Some(p) => p,
            None => return Ok(container_state.status),
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

        let provider = self.require_provider()?;

        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

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
        let mut state = self.state.write().await;
        if let Some(cs) = state.get_mut(id) {
            cs.status = status;
        }
        state.save()?;
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
        let container = Container::from_config(&state.config_path)?;
        Ok(container.devcontainer)
    }

    /// Discover all devcontainers from the current provider
    /// Includes containers not managed by devc (e.g., VS Code-created)
    pub async fn discover(&self) -> Result<Vec<DiscoveredContainer>> {
        let provider = self.require_provider()?;
        provider.discover_devcontainers().await.map_err(Into::into)
    }

    /// Adopt an existing devcontainer into devc management
    /// This creates a state entry for a container that was created outside devc
    pub async fn adopt(
        &self,
        container_id: &str,
        workspace_path: Option<&str>,
    ) -> Result<ContainerState> {
        let provider = self.require_provider()?;
        let provider_type = self
            .provider_type()
            .ok_or_else(|| CoreError::NotConnected("Cannot adopt: no provider available".to_string()))?;

        // Inspect the container to get details
        let details = provider
            .inspect(&ContainerId::new(container_id))
            .await?;

        // Determine workspace path
        let workspace = if let Some(path) = workspace_path {
            std::path::PathBuf::from(path)
        } else {
            // Try to detect from mounts or labels
            let from_labels = details.labels.get("devcontainer.local_folder");
            if let Some(path) = from_labels {
                std::path::PathBuf::from(path)
            } else {
                // Fall back to current directory
                std::env::current_dir()?
            }
        };

        // Find devcontainer.json if it exists
        let config_path = find_devcontainer_config(&workspace)?;

        // Determine container name
        let name = if !details.name.is_empty() {
            details.name.clone()
        } else {
            workspace
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "adopted".to_string())
        };

        // Check if already managed
        let state = self.state.read().await;
        if let Some(existing) = state.find_by_name(&name) {
            return Err(CoreError::ContainerExists(existing.name.clone()));
        }
        drop(state);

        // Determine status from container status
        let status = match details.status {
            ContainerStatus::Running => DevcContainerStatus::Running,
            ContainerStatus::Exited | ContainerStatus::Dead => DevcContainerStatus::Stopped,
            ContainerStatus::Created | ContainerStatus::Paused => DevcContainerStatus::Created,
            _ => DevcContainerStatus::Stopped,
        };

        // Create state entry
        let mut container_state = ContainerState::new(
            name,
            provider_type,
            config_path,
            workspace,
        );
        container_state.container_id = Some(container_id.to_string());
        container_state.image_id = Some(details.image_id.clone());
        container_state.status = status;

        // Save state
        {
            let mut state = self.state.write().await;
            state.add(container_state.clone());
            state.save()?;
        }

        Ok(container_state)
    }
}

/// Find devcontainer.json config file in a workspace
fn find_devcontainer_config(workspace: &std::path::Path) -> Result<std::path::PathBuf> {
    // Check standard locations
    let devcontainer_dir = workspace.join(".devcontainer/devcontainer.json");
    if devcontainer_dir.exists() {
        return Ok(devcontainer_dir);
    }

    let devcontainer_root = workspace.join(".devcontainer.json");
    if devcontainer_root.exists() {
        return Ok(devcontainer_root);
    }

    // If not found, return a default path (will be created later if needed)
    Ok(devcontainer_dir)
}

/// Convert a slice of PathBuf compose files to owned Strings and borrowed &str refs.
///
/// Returns (owned, refs) where `refs` borrows from `owned`.
/// Caller must keep `owned` alive while using `refs`.
fn compose_file_strs(files: &[std::path::PathBuf]) -> Vec<String> {
    files.iter().map(|f| f.to_string_lossy().to_string()).collect()
}

/// Helper to send progress messages
/// Extract merged feature properties from container state metadata.
fn get_feature_properties(state: &ContainerState) -> features::MergedFeatureProperties {
    state
        .metadata
        .get("feature_properties")
        .and_then(|json| serde_json::from_str(json).ok())
        .unwrap_or_default()
}

/// Merge feature remoteEnv with devcontainer.json remoteEnv.
/// Feature env provides a base; devcontainer.json wins on conflict.
fn merge_remote_env(
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

fn send_progress(progress: Option<&mpsc::UnboundedSender<String>>, msg: &str) {
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
        let state = StateStore::new();
        let mgr = ContainerManager::disconnected_for_testing(
            GlobalConfig::default(),
            state,
            "no runtime".to_string(),
        );
        // Any operation requiring a provider should fail
        let result = mgr.stop("nonexistent").await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("Not connected"));
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
    async fn test_sync_disconnected_returns_current() {
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

        let status = mgr.sync_status(&id).await.unwrap();
        assert_eq!(status, DevcContainerStatus::Running);
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
}
