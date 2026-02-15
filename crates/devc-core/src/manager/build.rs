//! Build and rebuild operations for ContainerManager

use crate::{features, CoreError, DevcContainerStatus, EnhancedBuildContext, Result};
use devc_config::ImageSource;
use devc_provider::ContainerProvider;
use std::path::Path;
use tokio::sync::mpsc;

use super::ContainerManager;

// Send a progress message to the channel, or log via tracing if no channel.
fn emit(progress: &Option<mpsc::UnboundedSender<String>>, msg: String) {
    if let Some(tx) = progress {
        let _ = tx.send(msg);
    } else {
        tracing::info!("{}", msg);
    }
}

// Dispatch a build to the provider, using progress-streaming or plain build.
async fn dispatch_build(
    provider: &dyn ContainerProvider,
    config: &devc_provider::BuildConfig,
    progress: &Option<mpsc::UnboundedSender<String>>,
) -> std::result::Result<devc_provider::ImageId, devc_provider::ProviderError> {
    if let Some(tx) = progress {
        provider.build_with_progress(config, tx.clone()).await
    } else {
        provider.build(config).await
    }
}

impl ContainerManager {
    /// Build a container image
    pub async fn build(&self, id: &str) -> Result<String> {
        self.build_inner(id, false, None).await
    }

    /// Build a container image with options
    pub async fn build_with_options(&self, id: &str, no_cache: bool) -> Result<String> {
        self.build_inner(id, no_cache, None).await
    }

    /// Build a container image with progress updates streamed to a channel
    pub async fn build_with_progress(
        &self,
        id: &str,
        no_cache: bool,
        progress: mpsc::UnboundedSender<String>,
    ) -> Result<String> {
        self.build_inner(id, no_cache, Some(progress)).await
    }

    /// Unified build implementation.
    ///
    /// When `progress` is Some, sends status messages to the channel and uses
    /// provider.build_with_progress(); otherwise logs via tracing::info and
    /// uses provider.build().
    pub(crate) async fn build_inner(
        &self,
        id: &str,
        no_cache: bool,
        progress: Option<mpsc::UnboundedSender<String>>,
    ) -> Result<String> {
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        let provider = self.require_container_provider(&container_state)?;

        // Load container config
        let container = self.load_container(&container_state.config_path)?;

        // Update status to building
        {
            let mut state = self.state.write().await;
            if let Some(cs) = state.get_mut(id) {
                cs.status = DevcContainerStatus::Building;
            }
        }
        self.save_state().await?;

        // Check if SSH injection is enabled
        let inject_ssh = self.global_config.defaults.ssh_enabled.unwrap_or(false);

        // Log SSH injection status
        if inject_ssh {
            emit(
                &progress,
                "SSH support: Injecting dropbear into image...".to_string(),
            );
        } else {
            emit(
                &progress,
                "SSH support: Disabled (not injecting dropbear)".to_string(),
            );
        }

        // Resolve devcontainer features
        let config_dir = container
            .config_path
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf();
        let progress_for_features = progress.clone();
        let resolved_features = if let Some(ref feature_map) = container.devcontainer.features {
            features::resolve_and_prepare_features(feature_map, &config_dir, &progress_for_features)
                .await?
        } else {
            vec![]
        };
        let has_features = !resolved_features.is_empty();
        let feature_properties = features::merge_feature_properties(&resolved_features);
        let remote_user = container
            .devcontainer
            .effective_user()
            .unwrap_or("root")
            .to_string();

        if has_features {
            emit(
                &progress,
                format!(
                    "Installing {} devcontainer feature(s)...",
                    resolved_features.len()
                ),
            );
        }

        // Check if we need to build or pull
        let image_id = match container.devcontainer.image_source() {
            ImageSource::Image(image) => {
                if has_features || inject_ssh {
                    emit(
                        &progress,
                        format!(
                            "Building enhanced image from {} (features: {}, SSH: {})",
                            image, has_features, inject_ssh
                        ),
                    );

                    let enhanced_ctx = if has_features {
                        EnhancedBuildContext::from_image_with_features(
                            &image,
                            &resolved_features,
                            inject_ssh,
                            &remote_user,
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
                        labels: std::collections::HashMap::from([
                            ("devc.managed".to_string(), "true".to_string()),
                            ("devc.project".to_string(), container.name.clone()),
                            ("devc.base_image".to_string(), image.clone()),
                        ]),
                        no_cache,
                        pull: true,
                    };

                    let result = dispatch_build(provider, &build_config, &progress).await;
                    match result {
                        Ok(id) => id.0,
                        Err(e) => {
                            self.set_status(id, DevcContainerStatus::Failed).await?;
                            return Err(e.into());
                        }
                    }
                } else {
                    emit(&progress, format!("Pulling image: {}", image));
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
                    emit(
                        &progress,
                        format!(
                            "Building enhanced image: {} (features: {}, SSH: {}, no_cache: {})",
                            build_config.tag, has_features, inject_ssh, no_cache
                        ),
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

                    let result = dispatch_build(provider, &build_config, &progress).await;
                    match result {
                        Ok(id) => id.0,
                        Err(e) => {
                            self.set_status(id, DevcContainerStatus::Failed).await?;
                            return Err(e.into());
                        }
                    }
                } else {
                    emit(
                        &progress,
                        format!(
                            "Building image: {} (no_cache: {})",
                            build_config.tag, no_cache
                        ),
                    );

                    let result = dispatch_build(provider, &build_config, &progress).await;
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
                emit(
                    &progress,
                    "Compose project: build will happen during 'up'".to_string(),
                );
                {
                    let mut state = self.state.write().await;
                    if let Some(cs) = state.get_mut(id) {
                        cs.image_id = Some("compose".to_string());
                        cs.status = DevcContainerStatus::Built;
                        if let Ok(props_json) = serde_json::to_string(&feature_properties) {
                            cs.metadata
                                .insert("feature_properties".to_string(), props_json);
                        }
                    }
                }
                self.save_state().await?;
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
                    cs.metadata
                        .insert("feature_properties".to_string(), props_json);
                }
            }
        }
        self.save_state().await?;

        Ok(image_id)
    }

    /// Rebuild a container, optionally migrating to current provider
    ///
    /// This will:
    /// 1. Stop and remove the runtime container (via down())
    /// 2. If provider changed: update state with new provider, clear image_id
    /// 3. Build image with optional --no-cache
    /// 4. Create and start the new container
    pub async fn rebuild(&self, id: &str, no_cache: bool) -> Result<()> {
        self.rebuild_inner(id, no_cache, None).await
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
        self.rebuild_inner(id, no_cache, Some(progress)).await
    }

    /// Unified rebuild implementation.
    ///
    /// When `progress` is Some, sends status messages to the channel;
    /// otherwise logs via tracing::info.
    async fn rebuild_inner(
        &self,
        id: &str,
        no_cache: bool,
        progress: Option<mpsc::UnboundedSender<String>>,
    ) -> Result<()> {
        let new_provider = self.provider_type().ok_or_else(|| {
            CoreError::NotConnected("Cannot rebuild: no provider available".to_string())
        })?;

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
            emit(&progress, "Stopping container...".to_string());
            self.down(id).await?;
        }

        // 2. Handle provider migration
        if provider_changed {
            emit(
                &progress,
                format!("Migrating provider: {} -> {}", old_provider, new_provider),
            );
            {
                let mut state = self.state.write().await;
                if let Some(cs) = state.get_mut(id) {
                    cs.provider = new_provider;
                    cs.image_id = None;
                    cs.container_id = None;
                    cs.status = DevcContainerStatus::Configured;
                }
            }
            self.save_state().await?;
        }

        // 3. Run initializeCommand on host before build (per spec)
        let container = self.load_container(&container_state.config_path)?;
        if let Some(ref cmd) = container.devcontainer.initialize_command {
            emit(
                &progress,
                "Running initializeCommand on host...".to_string(),
            );
            let output = progress.as_ref();
            crate::run_host_command(cmd, &container.workspace_path, output).await?;
        }

        // 4. Rebuild image
        self.build_inner(id, no_cache, progress.clone()).await?;

        // 5. Create and start container
        let progress_ref = progress.as_ref();
        self.up_with_progress(id, progress_ref, progress_ref)
            .await?;

        emit(&progress, "Build complete.".to_string());
        Ok(())
    }
}
