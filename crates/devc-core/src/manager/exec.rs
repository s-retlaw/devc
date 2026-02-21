//! Exec and shell operations for ContainerManager

use crate::{CoreError, DevcContainerStatus, Result};
use devc_provider::{ContainerId, ContainerProvider, ExecStream};
use std::time::Duration;

use super::{ContainerManager, ExecContext};

impl ContainerManager {
    async fn resolve_live_exec_container_id(
        &self,
        id: &str,
        provider: &dyn ContainerProvider,
        container_state: &crate::ContainerState,
    ) -> Result<ContainerId> {
        if let (Some(_project), Some(service)) = (
            container_state.compose_project.as_ref(),
            container_state.compose_service.as_deref(),
        ) {
            let container = self.load_container(&container_state.config_path)?;
            let compose_files = container.compose_files().ok_or_else(|| {
                CoreError::InvalidState("No dockerComposeFile specified".to_string())
            })?;
            let owned = super::compose_file_strs(&compose_files);
            let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
            let project_name = container.compose_project_name();
            let cid = self
                .resolve_running_compose_service_container_id(
                    provider,
                    &refs,
                    &project_name,
                    &container.workspace_path,
                    service,
                )
                .await?;

            if container_state.container_id.as_deref() != Some(cid.0.as_str()) {
                {
                    let mut state = self.state.write().await;
                    if let Some(cs) = state.get_mut(id) {
                        cs.container_id = Some(cid.0.clone());
                    }
                }
                self.save_state().await?;
            }
            return Ok(cid);
        }

        let stored = container_state
            .container_id
            .as_ref()
            .ok_or_else(|| CoreError::InvalidState("Container has no provider ID".into()))?;
        let cid = ContainerId::new(stored);
        if provider.inspect(&cid).await.is_err() {
            return Err(CoreError::InvalidState(format!(
                "Container '{}' is not inspectable/running",
                cid.0
            )));
        }
        Ok(cid)
    }

    fn is_container_missing_error(err: &CoreError) -> bool {
        let msg = err.to_string().to_lowercase();
        msg.contains("does not exist")
            || msg.contains("no such container")
            || msg.contains("no container with name or id")
    }

    /// Shared preamble for exec/shell operations.
    /// Validates the container is running, extracts provider ID, and loads feature properties.
    pub(crate) async fn prepare_exec(&self, id: &str) -> Result<ExecContext<'_>> {
        let container_state = {
            let state = self.state.read().await;
            state
                .get(id)
                .cloned()
                .ok_or_else(|| CoreError::ContainerNotFound(id.to_string()))?
        };

        let provider = self.require_container_provider(&container_state)?;

        if container_state.status != DevcContainerStatus::Running {
            return Err(CoreError::InvalidState(
                "Container is not running".to_string(),
            ));
        }

        let cid = self
            .resolve_live_exec_container_id(id, provider, &container_state)
            .await?;
        let feature_props = super::get_feature_properties(&container_state);

        Ok(ExecContext {
            provider,
            container_state,
            cid,
            feature_props,
        })
    }

    /// Refresh credential forwarding, logging results. Failures are non-fatal.
    /// Returns the GitHub CLI token if one was resolved, for injection into ExecConfig.
    pub(crate) async fn refresh_credentials(
        &self,
        provider: &dyn ContainerProvider,
        cid: &ContainerId,
        user: Option<&str>,
        workspace_path: &std::path::Path,
    ) -> Option<String> {
        match crate::credentials::setup_credentials(
            provider,
            cid,
            &self.global_config,
            user,
            workspace_path,
        )
        .await
        {
            Ok(status) => {
                if status.docker_registries > 0 || status.git_hosts > 0 {
                    tracing::info!(
                        "Credential forwarding: {} Docker registries, {} Git hosts",
                        status.docker_registries,
                        status.git_hosts
                    );
                }
                status.gh_token
            }
            Err(e) => {
                tracing::warn!("Credential forwarding setup failed (non-fatal): {}", e);
                None
            }
        }
    }

    /// Touch last-used timestamp and persist state.
    async fn touch_last_used(&self, id: &str) -> Result<()> {
        {
            let mut state = self.state.write().await;
            state.touch(id);
        }
        self.save_state().await
    }

    /// Execute a command in a container
    pub async fn exec(&self, id: &str, cmd: Vec<String>, tty: bool) -> Result<i64> {
        let result = self.exec_inner(id, cmd, tty).await?;
        Ok(result.exit_code)
    }

    /// Shared exec implementation
    async fn exec_inner(
        &self,
        id: &str,
        cmd: Vec<String>,
        tty: bool,
    ) -> Result<devc_provider::ExecResult> {
        let mut attempts = 0u8;
        let max_attempts = 2u8;
        loop {
            let ctx = self.prepare_exec(id).await?;

            // Try loading config for remoteEnv/user/workdir; fall back to a basic config
            // if the devcontainer.json is no longer accessible (e.g. tmp dir cleaned up)
            let (mut config, user_for_creds) =
                match self.load_container(&ctx.container_state.config_path) {
                    Ok(container) => {
                        let user = container
                            .devcontainer
                            .effective_user()
                            .map(|s| s.to_string());
                        (
                            container.exec_config_with_feature_env(
                                cmd.clone(),
                                tty,
                                tty,
                                ctx.feature_props.remote_env_option(),
                            ),
                            user,
                        )
                    }
                    Err(_) => {
                        let mut env = std::collections::HashMap::new();
                        env.insert("TERM".to_string(), "xterm-256color".to_string());
                        env.insert("COLORTERM".to_string(), "truecolor".to_string());
                        env.insert("LANG".to_string(), "C.UTF-8".to_string());
                        env.insert("LC_ALL".to_string(), "C.UTF-8".to_string());
                        (
                            devc_provider::ExecConfig {
                                cmd: cmd.clone(),
                                env,
                                working_dir: None,
                                user: None,
                                tty,
                                stdin: tty,
                                privileged: false,
                            },
                            None,
                        )
                    }
                };

            let gh_token = self
                .refresh_credentials(
                    ctx.provider,
                    &ctx.cid,
                    user_for_creds.as_deref(),
                    &ctx.container_state.workspace_path,
                )
                .await;
            if let Some(token) = gh_token {
                config.env.insert("GH_TOKEN".to_string(), token);
            }

            match ctx.provider.exec(&ctx.cid, &config).await {
                Ok(result) => {
                    self.touch_last_used(id).await?;
                    return Ok(result);
                }
                Err(e) => {
                    let core_err: CoreError = e.into();
                    attempts += 1;
                    if attempts < max_attempts && Self::is_container_missing_error(&core_err) {
                        tracing::warn!(
                            "exec target '{}' vanished, retrying with re-resolved compose service",
                            ctx.cid.0
                        );
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    }
                    return Err(core_err);
                }
            }
        }
    }

    /// Execute a command interactively with PTY
    pub async fn exec_interactive(&self, id: &str, cmd: Vec<String>) -> Result<ExecStream> {
        let ctx = self.prepare_exec(id).await?;
        let container = self.load_container(&ctx.container_state.config_path)?;

        let gh_token = self
            .refresh_credentials(
                ctx.provider,
                &ctx.cid,
                container.devcontainer.effective_user(),
                &ctx.container_state.workspace_path,
            )
            .await;

        let mut config = container.exec_config_with_feature_env(
            cmd,
            true,
            true,
            ctx.feature_props.remote_env_option(),
        );
        if let Some(token) = gh_token {
            config.env.insert("GH_TOKEN".to_string(), token);
        }
        let stream = ctx.provider.exec_interactive(&ctx.cid, &config).await?;

        self.touch_last_used(id).await?;

        Ok(stream)
    }

    /// Open an interactive shell in a container
    pub async fn shell(&self, id: &str) -> Result<ExecStream> {
        let ctx = self.prepare_exec(id).await?;
        let container = self.load_container(&ctx.container_state.config_path)?;

        let gh_token = self
            .refresh_credentials(
                ctx.provider,
                &ctx.cid,
                container.devcontainer.effective_user(),
                &ctx.container_state.workspace_path,
            )
            .await;

        let mut config =
            container.shell_config_with_feature_env(ctx.feature_props.remote_env_option());
        if let Some(token) = gh_token {
            config.env.insert("GH_TOKEN".to_string(), token);
        }
        let stream = ctx.provider.exec_interactive(&ctx.cid, &config).await?;

        self.touch_last_used(id).await?;

        Ok(stream)
    }
}
