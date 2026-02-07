//! Container manager - coordinates all container operations

use crate::{
    run_lifecycle_command_with_env, Container, ContainerState, CoreError, DevcContainerStatus,
    DotfilesManager, EnhancedBuildContext, Result, SshManager, StateStore,
};
use devc_config::{GlobalConfig, ImageSource};
use devc_provider::{
    ContainerId, ContainerProvider, ContainerStatus, DiscoveredContainer, ExecStream, LogConfig,
    ProviderType,
};
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

        // Check if already exists
        if let Some(existing) = state.find_by_workspace(&container.workspace_path) {
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

        // Check if we need to build or pull
        let image_id = match container.devcontainer.image_source() {
            ImageSource::Image(image) => {
                if inject_ssh {
                    // Build an enhanced image with dropbear pre-installed
                    tracing::info!(
                        "Building enhanced image from {} (with SSH support)",
                        image
                    );
                    let enhanced_ctx = EnhancedBuildContext::from_image(&image)?;

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
                    // Just pull the image directly (no SSH support)
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

                if inject_ssh {
                    // Create enhanced build context with dropbear appended
                    tracing::info!(
                        "Building enhanced image: {} (with SSH support, no_cache: {})",
                        build_config.tag,
                        no_cache
                    );
                    let enhanced_ctx = EnhancedBuildContext::from_dockerfile(
                        &build_config.context,
                        &build_config.dockerfile,
                    )?;

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
                self.set_status(id, DevcContainerStatus::Failed).await?;
                return Err(CoreError::InvalidState(
                    "Docker Compose not yet supported".to_string(),
                ));
            }
            ImageSource::None => {
                self.set_status(id, DevcContainerStatus::Failed).await?;
                return Err(CoreError::InvalidState(
                    "No image source specified".to_string(),
                ));
            }
        };

        // Update state with image ID
        {
            let mut state = self.state.write().await;
            if let Some(cs) = state.get_mut(id) {
                cs.image_id = Some(image_id.clone());
                cs.status = DevcContainerStatus::Built;
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
        let create_config = container.create_config(image_id);

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

        if !container_state.can_start() {
            return Err(CoreError::InvalidState(format!(
                "Container cannot be started in {} state",
                container_state.status
            )));
        }

        let container_id = container_state.container_id.as_ref().ok_or_else(|| {
            CoreError::InvalidState("Container not created yet".to_string())
        })?;

        provider.start(&ContainerId::new(container_id)).await?;

        // Update status
        self.set_status(id, DevcContainerStatus::Running).await?;

        // Ensure SSH daemon is running if SSH was set up for this container
        if container_state.metadata.get("ssh_available").map(|v| v == "true").unwrap_or(false) {
            self.ensure_ssh_daemon_running(&ContainerId::new(container_id)).await?;
        }

        // Run post-start commands
        let container = Container::from_config(&container_state.config_path)?;
        if let Some(ref cmd) = container.devcontainer.post_start_command {
            run_lifecycle_command_with_env(
                provider,
                &ContainerId::new(container_id),
                cmd,
                container.devcontainer.effective_user(),
                container.devcontainer.workspace_folder.as_deref(),
                container.devcontainer.remote_env.as_ref(),
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

    /// Stop a container
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

        // Update state: keep image but clear container_id, reset status to Built (or Configured if no image)
        {
            let mut state = self.state.write().await;
            if let Some(cs) = state.get_mut(id) {
                cs.container_id = None;
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
        self.up_with_progress(id, Some(&progress)).await?;

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

        // Check if we need to build or pull
        let image_id = match container.devcontainer.image_source() {
            ImageSource::Image(image) => {
                if inject_ssh {
                    let _ = progress.send(format!(
                        "Building enhanced image from {} (with SSH support)",
                        image
                    ));
                    let enhanced_ctx = EnhancedBuildContext::from_image(&image)?;

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

                if inject_ssh {
                    let _ = progress.send(format!(
                        "Building enhanced image: {} (with SSH support, no_cache: {})",
                        build_config.tag, no_cache
                    ));
                    let enhanced_ctx = EnhancedBuildContext::from_dockerfile(
                        &build_config.context,
                        &build_config.dockerfile,
                    )?;

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
                self.set_status(id, DevcContainerStatus::Failed).await?;
                return Err(CoreError::InvalidState(
                    "Docker Compose not yet supported".to_string(),
                ));
            }
            ImageSource::None => {
                self.set_status(id, DevcContainerStatus::Failed).await?;
                return Err(CoreError::InvalidState(
                    "No image source specified".to_string(),
                ));
            }
        };

        // Update state with image ID
        {
            let mut state = self.state.write().await;
            if let Some(cs) = state.get_mut(id) {
                cs.image_id = Some(image_id.clone());
                cs.status = DevcContainerStatus::Built;
            }
            state.save()?;
        }

        Ok(image_id)
    }

    /// Build, create, and start a container (full lifecycle)
    pub async fn up(&self, id: &str) -> Result<()> {
        self.up_with_progress(id, None).await
    }

    /// Build, create, and start a container with progress updates
    pub async fn up_with_progress(
        &self,
        id: &str,
        progress: Option<&mpsc::UnboundedSender<String>>,
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
        {
            let container = Container::from_config(&container_state.config_path)?;
            if let Some(ref cmd) = container.devcontainer.initialize_command {
                send_progress(progress, "Running initializeCommand on host...");
                crate::run_host_command(cmd, &container.workspace_path)?;
            }
            if let Some(ref wait_for) = container.devcontainer.wait_for {
                tracing::info!("waitFor is set to '{}' (async lifecycle deferral not yet implemented)", wait_for);
            }
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

        // Load container config for lifecycle commands
        let container = Container::from_config(&container_state.config_path)?;

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
            if let Some(ref cmd) = container.devcontainer.on_create_command {
                send_progress(progress, "Running onCreate command...");
                // Start the container first for onCreate
                provider.start(&container_id).await?;

                run_lifecycle_command_with_env(
                    provider,
                    &container_id,
                    cmd,
                    container.devcontainer.effective_user(),
                    container.devcontainer.workspace_folder.as_deref(),
                    container.devcontainer.remote_env.as_ref(),
                )
                .await?;
            }

            // Run updateContentCommand (between onCreate and postCreate per spec)
            if let Some(ref cmd) = container.devcontainer.update_content_command {
                send_progress(progress, "Running updateContentCommand...");
                let details = provider.inspect(&container_id).await?;
                if details.status != ContainerStatus::Running {
                    provider.start(&container_id).await?;
                }

                run_lifecycle_command_with_env(
                    provider,
                    &container_id,
                    cmd,
                    container.devcontainer.effective_user(),
                    container.devcontainer.workspace_folder.as_deref(),
                    container.devcontainer.remote_env.as_ref(),
                )
                .await?;
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
                    provider,
                    &container_id,
                    cmd,
                    container.devcontainer.effective_user(),
                    container.devcontainer.workspace_folder.as_deref(),
                    container.devcontainer.remote_env.as_ref(),
                )
                .await?;
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

        // Start if not running
        let details = provider.inspect(&container_id).await?;
        if details.status != ContainerStatus::Running {
            send_progress(progress, "Starting container...");
            self.start(id).await?;
        } else {
            self.set_status(id, DevcContainerStatus::Running).await?;

            // Ensure SSH daemon is running even if container was already up
            let container_state = {
                let state = self.state.read().await;
                state.get(id).cloned().unwrap()
            };
            if container_state.metadata.get("ssh_available").map(|v| v == "true").unwrap_or(false) {
                self.ensure_ssh_daemon_running(&container_id).await?;
            }
        }

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
        if let Some(ref cmd) = container.devcontainer.post_attach_command {
            let container_id = container_state.container_id.as_ref().ok_or_else(|| {
                CoreError::InvalidState("Container not created yet".to_string())
            })?;
            run_lifecycle_command_with_env(
                provider,
                &ContainerId::new(container_id),
                cmd,
                container.devcontainer.effective_user(),
                container.devcontainer.workspace_folder.as_deref(),
                container.devcontainer.remote_env.as_ref(),
            )
            .await?;
        }

        Ok(())
    }

    /// Execute a command in a container
    pub async fn exec(&self, id: &str, cmd: Vec<String>, tty: bool) -> Result<i64> {
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
        let container = Container::from_config(&container_state.config_path)?;

        let config = container.exec_config(cmd, tty, tty);
        let result = provider
            .exec(&ContainerId::new(container_id), &config)
            .await?;

        // Update last used
        {
            let mut state = self.state.write().await;
            state.touch(id);
            state.save()?;
        }

        Ok(result.exit_code)
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
        let container = Container::from_config(&container_state.config_path)?;

        let config = container.exec_config(cmd, true, true);
        let stream = provider
            .exec_interactive(&ContainerId::new(container_id), &config)
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
        let container = Container::from_config(&container_state.config_path)?;

        let config = container.shell_config();
        let stream = provider
            .exec_interactive(&ContainerId::new(container_id), &config)
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

/// Helper to send progress messages
fn send_progress(progress: Option<&mpsc::UnboundedSender<String>>, msg: &str) {
    if let Some(tx) = progress {
        let _ = tx.send(msg.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use devc_provider::{ContainerStatus, ProviderError, ProviderType};

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
    async fn test_build_compose_unsupported() {
        let workspace = create_test_workspace();
        // Write a compose-based devcontainer.json
        std::fs::write(
            workspace.path().join(".devcontainer/devcontainer.json"),
            r#"{"dockerComposeFile": "docker-compose.yml", "service": "app"}"#,
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
        assert!(err_msg.contains("Compose"));
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
    async fn test_start_invalid_state_fails() {
        let workspace = create_test_workspace();
        let mock = MockProvider::new(ProviderType::Docker);

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
}
