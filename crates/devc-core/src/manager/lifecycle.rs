//! Lifecycle command execution for ContainerManager

use crate::{
    run_feature_lifecycle_commands, run_lifecycle_command_with_env, Container, CoreError,
    DotfilesManager, Result, SshManager,
};
use devc_provider::{ContainerId, ContainerProvider, ContainerStatus};
use tokio::sync::mpsc;

use super::{get_feature_properties, merge_remote_env, send_progress, ContainerManager};

impl ContainerManager {
    /// Run first-create lifecycle commands on a container.
    ///
    /// This runs (in order):
    /// 1. Feature onCreateCommands
    /// 2. onCreateCommand
    /// 3. Feature updateContentCommands
    /// 4. updateContentCommand
    /// 5. Feature postCreateCommands
    /// 6. postCreateCommand
    /// 7. SSH setup (if enabled)
    /// 8. Dotfiles injection
    ///
    /// Used by both `up()` for newly created containers and `adopt()` for running containers.
    pub(crate) async fn run_first_create_lifecycle(
        &self,
        id: &str,
        container: &Container,
        provider: &dyn ContainerProvider,
        container_id: &ContainerId,
        progress: Option<&mpsc::UnboundedSender<String>>,
    ) -> Result<()> {
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };
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
            let details = provider.inspect(container_id).await?;
            if details.status != ContainerStatus::Running {
                provider.start(container_id).await?;
            }
            run_feature_lifecycle_commands(
                provider,
                container_id,
                &feature_props.on_create_commands,
                user,
                workspace_folder,
                remote_env,
            )
            .await?;
        }

        if let Some(ref cmd) = container.devcontainer.on_create_command {
            send_progress(progress, "Running onCreate command...");
            let details = provider.inspect(container_id).await?;
            if details.status != ContainerStatus::Running {
                provider.start(container_id).await?;
            }
            run_lifecycle_command_with_env(
                provider,
                container_id,
                cmd,
                user,
                workspace_folder,
                remote_env,
            )
            .await?;
        }

        // Feature updateContentCommands run first (per spec)
        if !feature_props.update_content_commands.is_empty() {
            send_progress(progress, "Running feature updateContentCommand(s)...");
            let details = provider.inspect(container_id).await?;
            if details.status != ContainerStatus::Running {
                provider.start(container_id).await?;
            }
            run_feature_lifecycle_commands(
                provider,
                container_id,
                &feature_props.update_content_commands,
                user,
                workspace_folder,
                remote_env,
            )
            .await?;
        }

        if let Some(ref cmd) = container.devcontainer.update_content_command {
            send_progress(progress, "Running updateContentCommand...");
            let details = provider.inspect(container_id).await?;
            if details.status != ContainerStatus::Running {
                provider.start(container_id).await?;
            }
            run_lifecycle_command_with_env(
                provider,
                container_id,
                cmd,
                user,
                workspace_folder,
                remote_env,
            )
            .await?;
        }

        // Feature postCreateCommands run first (per spec)
        if !feature_props.post_create_commands.is_empty() {
            send_progress(progress, "Running feature postCreateCommand(s)...");
            let details = provider.inspect(container_id).await?;
            if details.status != ContainerStatus::Running {
                provider.start(container_id).await?;
            }
            run_feature_lifecycle_commands(
                provider,
                container_id,
                &feature_props.post_create_commands,
                user,
                workspace_folder,
                remote_env,
            )
            .await?;
        }

        if let Some(ref cmd) = container.devcontainer.post_create_command {
            send_progress(progress, "Running postCreateCommand...");
            let details = provider.inspect(container_id).await?;
            if details.status != ContainerStatus::Running {
                provider.start(container_id).await?;
            }
            run_lifecycle_command_with_env(
                provider,
                container_id,
                cmd,
                user,
                workspace_folder,
                remote_env,
            )
            .await?;
        }

        // Setup SSH if enabled (for proper TTY/resize support)
        if self.global_config.defaults.ssh_enabled.unwrap_or(false) {
            send_progress(progress, "Setting up SSH...");
            let details = provider.inspect(container_id).await?;
            if details.status != ContainerStatus::Running {
                provider.start(container_id).await?;
            }

            let ssh_manager = SshManager::new()?;
            ssh_manager.ensure_keys_exist()?;

            let user = container.devcontainer.effective_user();
            match ssh_manager
                .setup_container(provider, container_id, user)
                .await
            {
                Ok(()) => {
                    tracing::info!("SSH setup completed for container");
                    {
                        let mut state = self.state.write().await;
                        if let Some(cs) = state.get_mut(id) {
                            cs.metadata
                                .insert("ssh_available".to_string(), "true".to_string());
                            if let Some(u) = user {
                                cs.metadata.insert("remote_user".to_string(), u.to_string());
                            }
                        }
                    }
                    self.save_state().await?;
                }
                Err(e) => {
                    tracing::warn!("SSH setup failed (will use exec fallback): {}", e);
                    {
                        let mut state = self.state.write().await;
                        if let Some(cs) = state.get_mut(id) {
                            cs.metadata
                                .insert("ssh_available".to_string(), "false".to_string());
                        }
                    }
                    self.save_state().await?;
                }
            }
        }

        // Inject dotfiles
        let dotfiles_manager = if let Some(ref dotfiles_config) = container.devcontainer.dotfiles {
            DotfilesManager::from_devcontainer_config(dotfiles_config, &self.global_config)
        } else {
            DotfilesManager::from_global_config(&self.global_config)
        };

        if dotfiles_manager.is_configured() {
            send_progress(progress, "Installing dotfiles...");
            dotfiles_manager
                .inject_with_progress(
                    provider,
                    container_id,
                    container.devcontainer.effective_user(),
                    progress,
                )
                .await?;
        }

        Ok(())
    }

    /// Run postAttachCommand for a container (if configured)
    pub async fn run_post_attach_command(&self, id: &str) -> Result<()> {
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        let provider = self.require_container_provider(&container_state)?;

        let container = self.load_container(&container_state.config_path)?;
        let container_id_str = container_state
            .container_id
            .as_ref()
            .ok_or_else(|| CoreError::InvalidState("Container not created yet".to_string()))?;
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
}
