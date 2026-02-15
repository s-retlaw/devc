//! Docker Compose orchestration for ContainerManager

use crate::{
    features, run_feature_lifecycle_commands, run_lifecycle_command_with_env, Container,
    CoreError, DevcContainerStatus, Result, SshManager,
};
use devc_provider::ContainerProvider;
use std::path::Path;
use tokio::sync::mpsc;

use super::{compose_file_strs, merge_remote_env, send_progress, ContainerManager};

impl ContainerManager {
    /// Handle Docker Compose `up` flow
    ///
    /// 1. Run `compose up -d --build` to start all services
    /// 2. Find the dev service container ID via `compose ps`
    /// 3. Store compose metadata in state
    /// 4. Run lifecycle commands targeting the dev service container
    pub(crate) async fn up_compose(
        &self,
        id: &str,
        container: &Container,
        container_state: &crate::ContainerState,
        provider: &dyn ContainerProvider,
        progress: Option<&mpsc::UnboundedSender<String>>,
        output: Option<&mpsc::UnboundedSender<String>>,
    ) -> Result<()> {
        // initializeCommand runs on host before first compose up (per spec)
        if container_state.container_id.is_none() {
            if let Some(ref cmd) = container.devcontainer.initialize_command {
                send_progress(progress, "Running initializeCommand on host...");
                crate::run_host_command(cmd, &container.workspace_path, output).await?;
            }
        }

        let compose_files = container.compose_files().ok_or_else(|| {
            CoreError::InvalidState("No dockerComposeFile specified".to_string())
        })?;
        let service_name = container.compose_service().ok_or_else(|| {
            CoreError::InvalidState("No service specified for compose project".to_string())
        })?;
        let project_name = container.compose_project_name();

        let mut owned = compose_file_strs(&compose_files);

        // Resolve devcontainer features for compose override + exec-based install
        let config_dir = container
            .config_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
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
                service_name,
                &feature_props,
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
                provider,
                &container_id,
                &resolved_features,
                remote_user,
                progress,
            )
            .await?;
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
        }
        self.save_state().await?;

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
                provider,
                &container_id,
                &feature_props.on_create_commands,
                user,
                workspace_folder,
                remote_env,
            )
            .await?;
        }

        if let Some(ref cmd) = container.devcontainer.on_create_command {
            send_progress(progress, "Running onCreate command...");
            run_lifecycle_command_with_env(
                provider, &container_id, cmd, user, workspace_folder, remote_env,
            )
            .await?;
        }

        if !feature_props.update_content_commands.is_empty() {
            send_progress(progress, "Running feature updateContentCommand(s)...");
            run_feature_lifecycle_commands(
                provider,
                &container_id,
                &feature_props.update_content_commands,
                user,
                workspace_folder,
                remote_env,
            )
            .await?;
        }

        if let Some(ref cmd) = container.devcontainer.update_content_command {
            send_progress(progress, "Running updateContentCommand...");
            run_lifecycle_command_with_env(
                provider, &container_id, cmd, user, workspace_folder, remote_env,
            )
            .await?;
        }

        if !feature_props.post_create_commands.is_empty() {
            send_progress(progress, "Running feature postCreateCommand(s)...");
            run_feature_lifecycle_commands(
                provider,
                &container_id,
                &feature_props.post_create_commands,
                user,
                workspace_folder,
                remote_env,
            )
            .await?;
        }

        if let Some(ref cmd) = container.devcontainer.post_create_command {
            send_progress(progress, "Running postCreateCommand...");
            run_lifecycle_command_with_env(
                provider, &container_id, cmd, user, workspace_folder, remote_env,
            )
            .await?;
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
                    {
                        let mut state = self.state.write().await;
                        if let Some(cs) = state.get_mut(id) {
                            cs.metadata
                                .insert("ssh_available".to_string(), "true".to_string());
                            if let Some(u) = user {
                                cs.metadata
                                    .insert("remote_user".to_string(), u.to_string());
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

        if !feature_props.post_start_commands.is_empty() {
            send_progress(progress, "Running feature postStartCommand(s)...");
            run_feature_lifecycle_commands(
                provider,
                &container_id,
                &feature_props.post_start_commands,
                user,
                workspace_folder,
                remote_env,
            )
            .await?;
        }

        if let Some(ref cmd) = container.devcontainer.post_start_command {
            send_progress(progress, "Running postStartCommand...");
            run_lifecycle_command_with_env(
                provider, &container_id, cmd, user, workspace_folder, remote_env,
            )
            .await?;
        }

        send_progress(progress, "Compose project started!");
        Ok(())
    }
}
