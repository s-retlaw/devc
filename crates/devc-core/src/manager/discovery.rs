//! Container discovery and adoption for ContainerManager

use crate::{
    Container, ContainerState, CoreError, DevcContainerStatus, Result,
};
use devc_provider::{
    ContainerId, ContainerStatus, DevcontainerSource, DiscoveredContainer, ProviderType,
};
use std::path::{Path, PathBuf};

use super::ContainerManager;

impl ContainerManager {
    /// Initialize a new container from a specific config path.
    /// Returns Ok(None) if the config is already registered (not an error).
    pub async fn init_from_config(&self, config_path: &Path) -> Result<Option<ContainerState>> {
        let provider_type = self.provider_type().ok_or_else(|| {
            CoreError::NotConnected("Cannot init: no provider available".to_string())
        })?;

        let container = self.load_container(config_path)?;

        let container_state = {
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
            container_state
        };
        self.save_state().await?;

        Ok(Some(container_state))
    }

    /// Auto-discover all devcontainer.json configs in a workspace directory
    /// and register any that aren't already tracked.
    /// Returns the list of newly registered container states.
    pub async fn auto_discover_configs(
        &self,
        workspace_dir: &Path,
    ) -> Result<Vec<ContainerState>> {
        use devc_config::DevContainerConfig;

        let all_configs = DevContainerConfig::load_all_from_dir(workspace_dir);
        let mut newly_registered = Vec::new();

        for (_config, config_path) in all_configs {
            match self.init_from_config(&config_path).await {
                Ok(Some(cs)) => newly_registered.push(cs),
                Ok(None) => {} // already registered
                Err(e) => {
                    tracing::warn!("Skipping config {}: {}", config_path.display(), e);
                }
            }
        }

        Ok(newly_registered)
    }

    /// Find devcontainer.json configs on disk that are NOT already registered
    /// in the state store. Returns (name, config_path, workspace_path) tuples.
    /// Does NOT register anything — the results are ephemeral.
    pub async fn find_unregistered_configs(
        &self,
        workspace_dir: &Path,
    ) -> Vec<(String, PathBuf, PathBuf)> {
        use devc_config::DevContainerConfig;

        let all_configs = DevContainerConfig::load_all_from_dir(workspace_dir);
        let state = self.state.read().await;
        let mut unregistered = Vec::new();

        for (_config, config_path) in all_configs {
            if state.find_by_config_path(&config_path).is_some() {
                continue; // already registered
            }
            match self.load_container(&config_path) {
                Ok(container) => {
                    unregistered.push((
                        container.name.clone(),
                        container.config_path.clone(),
                        container.workspace_path.clone(),
                    ));
                }
                Err(e) => {
                    tracing::warn!("Skipping config {}: {}", config_path.display(), e);
                }
            }
        }

        unregistered
    }

    /// Discover all devcontainers from the current provider
    /// Includes containers not managed by devc (e.g., VS Code-created)
    pub async fn discover(&self) -> Result<Vec<DiscoveredContainer>> {
        let provider = self.require_provider()?;
        provider.discover_devcontainers().await.map_err(Into::into)
    }

    /// Discover devcontainers across all available providers (Docker + Podman)
    /// Returns a merged, deduplicated list of containers from every connected runtime.
    pub async fn discover_all(&self) -> Vec<DiscoveredContainer> {
        let available = devc_provider::detect_available_providers(&self.global_config).await;

        let mut all = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();

        for (provider_type, is_available) in &available {
            if !is_available {
                continue;
            }
            let provider =
                match devc_provider::create_provider(*provider_type, &self.global_config).await {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(
                            "Failed to create {} provider for discovery: {}",
                            provider_type,
                            e
                        );
                        continue;
                    }
                };
            match provider.discover_devcontainers().await {
                Ok(containers) => {
                    for c in containers {
                        if seen_ids.insert(c.id.0.clone()) {
                            all.push(c);
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Discovery failed on {}: {}", provider_type, e);
                }
            }
        }

        // Sort by created timestamp descending (newest first)
        all.sort_by(|a, b| b.created.cmp(&a.created));

        all
    }

    /// Adopt an existing devcontainer into devc management
    /// This creates a state entry for a container that was created outside devc
    pub async fn adopt(
        &self,
        container_id: &str,
        workspace_path: Option<&str>,
        source: DevcontainerSource,
        provider_type: ProviderType,
    ) -> Result<ContainerState> {
        let provider = self.require_provider_for(provider_type)?;

        // Inspect the container to get details
        let details = provider.inspect(&ContainerId::new(container_id)).await?;

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
        let mut container_state =
            ContainerState::new(name, provider_type, config_path, workspace);
        container_state.container_id = Some(container_id.to_string());
        container_state.image_id = Some(details.image_id.clone());
        container_state.status = status;
        container_state.source = source;

        // Extract remote_user and workspace_folder from devcontainer.json if available
        if container_state.config_path.exists() {
            if let Ok(c) = Container::from_config(&container_state.config_path) {
                if let Some(user) = c.devcontainer.effective_user() {
                    container_state
                        .metadata
                        .insert("remote_user".to_string(), user.to_string());
                }
                if let Some(ref wf) = c.devcontainer.workspace_folder {
                    container_state
                        .metadata
                        .insert("workspace_folder".to_string(), wf.clone());
                }
            }
        }

        // Fall back to detecting workspace_folder from bind mounts
        if !container_state.metadata.contains_key("workspace_folder") {
            if let Some(mount) = details
                .mounts
                .iter()
                .find(|m| m.mount_type == "bind" && m.destination.starts_with("/workspaces/"))
            {
                container_state
                    .metadata
                    .insert("workspace_folder".to_string(), mount.destination.clone());
            }
        }

        // Save state
        let state_id = container_state.id.clone();
        {
            let mut state = self.state.write().await;
            state.add(container_state.clone());
        }
        self.save_state().await?;

        // Run lifecycle commands if the container is running and has a valid config
        if container_state.status == DevcContainerStatus::Running
            && container_state.config_path.exists()
        {
            if let Ok(container) = self.load_container(&container_state.config_path) {
                let cid = ContainerId::new(container_id);

                // initializeCommand runs on host (per spec)
                if let Some(ref cmd) = container.devcontainer.initialize_command {
                    if let Err(e) =
                        crate::run_host_command(cmd, &container_state.workspace_path, None).await
                    {
                        tracing::warn!(
                            "initializeCommand failed during adopt (non-fatal): {}",
                            e
                        );
                    }
                }

                // Run first-create lifecycle (non-fatal — adopt succeeds even if lifecycle fails)
                if let Err(e) = self
                    .run_first_create_lifecycle(&state_id, &container, provider, &cid, None)
                    .await
                {
                    tracing::warn!(
                        "Lifecycle commands failed during adopt (non-fatal): {}",
                        e
                    );
                }

                // Start (runs postStartCommand, SSH daemon)
                if let Err(e) = self.start(&state_id).await {
                    tracing::warn!("Post-start phase failed during adopt (non-fatal): {}", e);
                }

                // Credentials setup
                if let Err(e) = crate::credentials::setup_credentials(
                    provider,
                    &cid,
                    &self.global_config,
                    container.devcontainer.effective_user(),
                    &container_state.workspace_path,
                )
                .await
                {
                    tracing::warn!(
                        "Credential forwarding failed during adopt (non-fatal): {}",
                        e
                    );
                }
            }
        }

        // Re-read state to capture any metadata updates from lifecycle
        let final_state = {
            let state = self.state.read().await;
            state.get(&state_id).cloned().unwrap_or(container_state)
        };

        Ok(final_state)
    }

    /// Remove a container from devc tracking without stopping or deleting the runtime container
    pub async fn forget(&self, id: &str) -> Result<()> {
        {
            let mut state = self.state.write().await;
            state.remove(id);
        }
        self.save_state().await?;
        Ok(())
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
