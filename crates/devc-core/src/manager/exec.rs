//! Exec and shell operations for ContainerManager

use crate::{CoreError, DevcContainerStatus, Result};
use devc_provider::{ContainerId, ContainerProvider, ExecStream};

use super::{ContainerManager, ExecContext};

impl ContainerManager {
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

        let container_id = container_state
            .container_id
            .as_ref()
            .ok_or_else(|| CoreError::InvalidState("Container has no provider ID".into()))?;
        let cid = ContainerId::new(container_id);
        let feature_props = super::get_feature_properties(&container_state);

        Ok(ExecContext {
            provider,
            container_state,
            cid,
            feature_props,
        })
    }

    /// Refresh credential forwarding, logging results. Failures are non-fatal.
    pub(crate) async fn refresh_credentials(
        &self,
        provider: &dyn ContainerProvider,
        cid: &ContainerId,
        user: Option<&str>,
        workspace_path: &std::path::Path,
    ) {
        match crate::credentials::setup_credentials(
            provider,
            cid,
            &self.global_config,
            user,
            workspace_path,
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
        let ctx = self.prepare_exec(id).await?;

        // Try loading config for remoteEnv/user/workdir; fall back to a basic config
        // if the devcontainer.json is no longer accessible (e.g. tmp dir cleaned up)
        let (config, user_for_creds) = match self.load_container(&ctx.container_state.config_path) {
            Ok(container) => {
                let user = container
                    .devcontainer
                    .effective_user()
                    .map(|s| s.to_string());
                (
                    container.exec_config_with_feature_env(
                        cmd,
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
                        cmd,
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

        self.refresh_credentials(
            ctx.provider,
            &ctx.cid,
            user_for_creds.as_deref(),
            &ctx.container_state.workspace_path,
        )
        .await;

        let result = ctx.provider.exec(&ctx.cid, &config).await?;

        self.touch_last_used(id).await?;

        Ok(result)
    }

    /// Execute a command interactively with PTY
    pub async fn exec_interactive(&self, id: &str, cmd: Vec<String>) -> Result<ExecStream> {
        let ctx = self.prepare_exec(id).await?;
        let container = self.load_container(&ctx.container_state.config_path)?;

        self.refresh_credentials(
            ctx.provider,
            &ctx.cid,
            container.devcontainer.effective_user(),
            &ctx.container_state.workspace_path,
        )
        .await;

        let config = container.exec_config_with_feature_env(
            cmd,
            true,
            true,
            ctx.feature_props.remote_env_option(),
        );
        let stream = ctx.provider.exec_interactive(&ctx.cid, &config).await?;

        self.touch_last_used(id).await?;

        Ok(stream)
    }

    /// Open an interactive shell in a container
    pub async fn shell(&self, id: &str) -> Result<ExecStream> {
        let ctx = self.prepare_exec(id).await?;
        let container = self.load_container(&ctx.container_state.config_path)?;

        self.refresh_credentials(
            ctx.provider,
            &ctx.cid,
            container.devcontainer.effective_user(),
            &ctx.container_state.workspace_path,
        )
        .await;

        let config = container.shell_config_with_feature_env(ctx.feature_props.remote_env_option());
        let stream = ctx.provider.exec_interactive(&ctx.cid, &config).await?;

        self.touch_last_used(id).await?;

        Ok(stream)
    }
}
