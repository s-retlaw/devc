//! Container manager - coordinates all container operations

use crate::{
    run_lifecycle_command, Container, ContainerState, CoreError, DevcContainerStatus,
    DotfilesManager, EnhancedBuildContext, Result, SshManager, StateStore,
};
use devc_config::{GlobalConfig, ImageSource};
use devc_provider::{
    ContainerId, ContainerProvider, ContainerStatus, ExecStream, ProviderType,
};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Main container manager
pub struct ContainerManager {
    /// Container provider
    provider: Box<dyn ContainerProvider>,
    /// State store
    state: Arc<RwLock<StateStore>>,
    /// Global configuration
    global_config: GlobalConfig,
}

/// Build progress callback
pub type BuildProgressCallback = Box<dyn Fn(&str) + Send + Sync>;

impl ContainerManager {
    /// Create a new container manager
    pub async fn new(provider: Box<dyn ContainerProvider>) -> Result<Self> {
        let global_config = GlobalConfig::load()?;
        let state = StateStore::load()?;

        Ok(Self {
            provider,
            state: Arc::new(RwLock::new(state)),
            global_config,
        })
    }

    /// Create with specific global config
    pub async fn with_config(
        provider: Box<dyn ContainerProvider>,
        global_config: GlobalConfig,
    ) -> Result<Self> {
        let state = StateStore::load()?;

        Ok(Self {
            provider,
            state: Arc::new(RwLock::new(state)),
            global_config,
        })
    }

    /// Get the provider type
    pub fn provider_type(&self) -> ProviderType {
        self.provider.info().provider_type
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
        let container = Container::from_workspace(workspace_path)?;

        let mut state = self.state.write().await;

        // Check if already exists
        if let Some(existing) = state.find_by_workspace(&container.workspace_path) {
            return Err(CoreError::ContainerExists(existing.name.clone()));
        }

        let container_state = ContainerState::new(
            container.name.clone(),
            self.provider_type(),
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
        let inject_ssh = self.global_config.defaults.ssh_enabled.unwrap_or(true);

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

                    let result = self.provider.build(&build_config).await;
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
                    let result = self.provider.pull(&image).await;
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

                    let result = self.provider.build(&build_config).await;
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

                    let result = self.provider.build(&build_config).await;
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

        let container_id = self.provider.create(&create_config).await?;

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

        self.provider.start(&ContainerId::new(container_id)).await?;

        // Update status
        self.set_status(id, DevcContainerStatus::Running).await?;

        // Run post-start commands
        let container = Container::from_config(&container_state.config_path)?;
        if let Some(ref cmd) = container.devcontainer.post_start_command {
            run_lifecycle_command(
                self.provider.as_ref(),
                &ContainerId::new(container_id),
                cmd,
                container.devcontainer.effective_user(),
                container.devcontainer.workspace_folder.as_deref(),
            )
            .await?;
        }

        Ok(())
    }

    /// Stop a container
    pub async fn stop(&self, id: &str) -> Result<()> {
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

        self.provider
            .stop(&ContainerId::new(container_id), Some(10))
            .await?;

        self.set_status(id, DevcContainerStatus::Stopped).await?;

        Ok(())
    }

    /// Remove a container
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

        // Remove the container if it exists
        if let Some(ref container_id) = container_state.container_id {
            let _ = self
                .provider
                .remove(&ContainerId::new(container_id), force)
                .await;
        }

        // Remove from state
        {
            let mut state = self.state.write().await;
            state.remove(id);
            state.save()?;
        }

        Ok(())
    }

    /// Build, create, and start a container (full lifecycle)
    pub async fn up(&self, id: &str) -> Result<()> {
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        // Build if needed
        if container_state.image_id.is_none() {
            self.build(id).await?;
        }

        // Create if needed
        let container_state = {
            let state = self.state.read().await;
            state.get(id).cloned().unwrap()
        };

        if container_state.container_id.is_none() {
            self.create(id).await?;
        }

        // Load container config for lifecycle commands
        let container = Container::from_config(&container_state.config_path)?;

        // Get the container ID
        let container_state = {
            let state = self.state.read().await;
            state.get(id).cloned().unwrap()
        };
        let container_id = ContainerId::new(container_state.container_id.as_ref().unwrap());

        // Run onCreate command if this is first create
        if container_state.status == DevcContainerStatus::Created {
            if let Some(ref cmd) = container.devcontainer.on_create_command {
                // Start the container first for onCreate
                self.provider.start(&container_id).await?;

                run_lifecycle_command(
                    self.provider.as_ref(),
                    &container_id,
                    cmd,
                    container.devcontainer.effective_user(),
                    container.devcontainer.workspace_folder.as_deref(),
                )
                .await?;
            }

            // Run postCreateCommand
            if let Some(ref cmd) = container.devcontainer.post_create_command {
                // Ensure container is started
                let details = self.provider.inspect(&container_id).await?;
                if details.status != ContainerStatus::Running {
                    self.provider.start(&container_id).await?;
                }

                run_lifecycle_command(
                    self.provider.as_ref(),
                    &container_id,
                    cmd,
                    container.devcontainer.effective_user(),
                    container.devcontainer.workspace_folder.as_deref(),
                )
                .await?;
            }

            // Setup SSH if enabled (for proper TTY/resize support)
            if self.global_config.defaults.ssh_enabled.unwrap_or(true) {
                // Ensure container is running for SSH setup
                let details = self.provider.inspect(&container_id).await?;
                if details.status != ContainerStatus::Running {
                    self.provider.start(&container_id).await?;
                }

                let ssh_manager = SshManager::new()?;
                ssh_manager.ensure_keys_exist()?;

                let user = container.devcontainer.effective_user();
                match ssh_manager
                    .setup_container(self.provider.as_ref(), &container_id, user)
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
                dotfiles_manager
                    .inject(
                        self.provider.as_ref(),
                        &container_id,
                        container.devcontainer.effective_user(),
                    )
                    .await?;
            }
        }

        // Start if not running
        let details = self.provider.inspect(&container_id).await?;
        if details.status != ContainerStatus::Running {
            self.start(id).await?;
        } else {
            self.set_status(id, DevcContainerStatus::Running).await?;
        }

        Ok(())
    }

    /// Execute a command in a container
    pub async fn exec(&self, id: &str, cmd: Vec<String>, tty: bool) -> Result<i64> {
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
        let result = self
            .provider
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
        let stream = self
            .provider
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
        let stream = self
            .provider
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
    pub async fn sync_status(&self, id: &str) -> Result<DevcContainerStatus> {
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        let new_status = if let Some(ref container_id) = container_state.container_id {
            match self.provider.inspect(&ContainerId::new(container_id)).await {
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

    /// Helper to set container status
    async fn set_status(&self, id: &str, status: DevcContainerStatus) -> Result<()> {
        let mut state = self.state.write().await;
        if let Some(cs) = state.get_mut(id) {
            cs.status = status;
        }
        state.save()?;
        Ok(())
    }
}
