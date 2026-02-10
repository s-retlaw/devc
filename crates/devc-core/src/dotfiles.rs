//! Dotfiles injection into containers

use crate::{CoreError, Result};
use devc_config::{DotfilesConfig, GlobalConfig};
use devc_provider::{ContainerId, ContainerProvider, ExecConfig};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::process::Command as TokioCommand;

/// POSIX shell-quote a string: wraps in single quotes, escaping embedded single quotes.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Expand `~` prefix to a concrete home directory path.
fn expand_home(path: &str, user: Option<&str>) -> String {
    if let Some(rest) = path.strip_prefix('~') {
        let home = match user {
            Some("root") | None => "/root".to_string(),
            Some(u) => format!("/home/{}", u),
        };
        format!("{}{}", home, rest)
    } else {
        path.to_string()
    }
}

/// Dotfiles manager for injecting dotfiles into containers
pub struct DotfilesManager {
    /// Source configuration
    config: DotfilesSource,
    /// Target path in container
    target_path: String,
    /// Install command to run after copying
    install_command: Option<String>,
}

/// Source of dotfiles
#[derive(Debug, Clone)]
pub enum DotfilesSource {
    /// Git repository URL
    Repository(String),
    /// Local directory path
    Local(PathBuf),
    /// No dotfiles configured
    None,
}

impl DotfilesManager {
    /// Create from global config
    pub fn from_global_config(config: &GlobalConfig) -> Self {
        let source = if let Some(ref repo) = config.defaults.dotfiles_repo {
            DotfilesSource::Repository(repo.clone())
        } else if let Some(ref local) = config.defaults.dotfiles_local {
            let path = shellexpand::tilde(local);
            DotfilesSource::Local(PathBuf::from(path.as_ref()))
        } else {
            DotfilesSource::None
        };

        Self {
            config: source,
            target_path: "~/.dotfiles".to_string(),
            install_command: None,
        }
    }

    /// Create from devcontainer dotfiles config
    pub fn from_devcontainer_config(config: &DotfilesConfig, global: &GlobalConfig) -> Self {
        let source = if let Some(ref repo) = config.repository {
            DotfilesSource::Repository(repo.clone())
        } else if let Some(ref local) = config.local_path {
            let path = shellexpand::tilde(local);
            DotfilesSource::Local(PathBuf::from(path.as_ref()))
        } else {
            // Fall back to global config
            return Self::from_global_config(global);
        };

        Self {
            config: source,
            target_path: config
                .target_path
                .clone()
                .unwrap_or_else(|| "~/.dotfiles".to_string()),
            install_command: config.install_command.clone(),
        }
    }

    /// Check if dotfiles are configured
    pub fn is_configured(&self) -> bool {
        !matches!(self.config, DotfilesSource::None)
    }

    /// Inject dotfiles into a container
    pub async fn inject(
        &self,
        provider: &dyn ContainerProvider,
        container_id: &ContainerId,
        user: Option<&str>,
    ) -> Result<()> {
        self.inject_with_progress(provider, container_id, user, None).await
    }

    /// Inject dotfiles into a container with progress updates
    pub async fn inject_with_progress(
        &self,
        provider: &dyn ContainerProvider,
        container_id: &ContainerId,
        user: Option<&str>,
        progress: Option<&tokio::sync::mpsc::UnboundedSender<String>>,
    ) -> Result<()> {
        if !self.is_configured() {
            tracing::debug!("No dotfiles configured, skipping injection");
            return Ok(());
        }

        match &self.config {
            DotfilesSource::Repository(url) => {
                send_progress(progress, "Cloning dotfiles repository...");
                self.inject_from_repo(provider, container_id, url, user, progress)
                    .await?;
            }
            DotfilesSource::Local(path) => {
                send_progress(progress, "Copying dotfiles...");
                self.inject_from_local(provider, container_id, path, user)
                    .await?;
            }
            DotfilesSource::None => {}
        }

        // Run install command if configured
        if let Some(ref cmd) = self.install_command {
            send_progress(progress, "Running dotfiles install command...");
            self.run_install_command(provider, container_id, cmd, user)
                .await?;
        } else {
            // Try to run default install scripts
            send_progress(progress, "Running dotfiles install script...");
            self.run_default_install(provider, container_id, user)
                .await?;
        }

        // Symlink standard dotfiles
        self.symlink_dotfiles(provider, container_id, user).await?;

        Ok(())
    }

    /// Clone dotfiles from a git repository.
    ///
    /// Tries cloning inside the container first. If that fails (e.g. no git
    /// installed, auth issues), falls back to cloning on the host and copying
    /// files into the container.
    async fn inject_from_repo(
        &self,
        provider: &dyn ContainerProvider,
        container_id: &ContainerId,
        url: &str,
        user: Option<&str>,
        progress: Option<&tokio::sync::mpsc::UnboundedSender<String>>,
    ) -> Result<()> {
        tracing::info!("Cloning dotfiles from {}", url);

        let target = expand_home(&self.target_path, user);
        let qt = shell_quote(&target);
        let qu = shell_quote(url);
        let cmd = format!(
            "if [ -d {qt} ]; then cd {qt} && git pull; else git clone {qu} {qt}; fi"
        );

        let config = ExecConfig {
            cmd: vec!["/bin/sh".to_string(), "-c".to_string(), cmd],
            env: HashMap::new(),
            working_dir: None,
            user: user.map(|s| s.to_string()),
            tty: false,
            stdin: false,
            privileged: false,
        };

        let result = provider.exec(container_id, &config).await?;
        if result.exit_code != 0 {
            tracing::warn!(
                "In-container git clone failed: {}. Falling back to host-side clone...",
                result.output
            );
            send_progress(progress, "Falling back to host-side clone...");
            self.inject_from_repo_host(provider, container_id, url, user)
                .await?;
        }

        Ok(())
    }

    /// Clone dotfiles on the host and copy into the container.
    async fn inject_from_repo_host(
        &self,
        provider: &dyn ContainerProvider,
        container_id: &ContainerId,
        url: &str,
        user: Option<&str>,
    ) -> Result<()> {
        tracing::info!("Cloning dotfiles on host from {}", url);

        let temp_dir = tempfile::tempdir().map_err(|e| {
            CoreError::DotfilesError(format!("Failed to create temp directory: {}", e))
        })?;
        let clone_path = temp_dir.path().join("dotfiles");

        let output = TokioCommand::new("git")
            .args(["clone", "--depth", "1", url])
            .arg(&clone_path)
            .output()
            .await
            .map_err(|e| {
                CoreError::DotfilesError(format!("Failed to run git on host: {}", e))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CoreError::DotfilesError(format!(
                "Host-side git clone failed: {}",
                stderr
            )));
        }

        self.inject_from_local(provider, container_id, &clone_path, user)
            .await
        // temp_dir is dropped here, cleaning up the clone
    }

    /// Copy dotfiles from local directory
    async fn inject_from_local(
        &self,
        provider: &dyn ContainerProvider,
        container_id: &ContainerId,
        path: &Path,
        user: Option<&str>,
    ) -> Result<()> {
        tracing::info!("Copying dotfiles from {:?}", path);

        if !path.exists() {
            return Err(CoreError::DotfilesError(format!(
                "Dotfiles directory not found: {:?}",
                path
            )));
        }

        // Create target directory first
        let container_target = expand_home(&self.target_path, user);
        let mkdir_cmd = format!("mkdir -p {}", shell_quote(&container_target));
        let config = ExecConfig {
            cmd: vec!["/bin/sh".to_string(), "-c".to_string(), mkdir_cmd],
            env: HashMap::new(),
            working_dir: None,
            user: user.map(|s| s.to_string()),
            tty: false,
            stdin: false,
            privileged: false,
        };
        provider.exec(container_id, &config).await?;

        // Copy files into container

        provider
            .copy_into(container_id, path, &container_target)
            .await?;

        Ok(())
    }

    /// Run the configured install command
    async fn run_install_command(
        &self,
        provider: &dyn ContainerProvider,
        container_id: &ContainerId,
        cmd: &str,
        user: Option<&str>,
    ) -> Result<()> {
        tracing::info!("Running dotfiles install command: {}", cmd);

        // Use cd in the shell command; target_path is quoted, cmd is intentionally unquoted
        let target = expand_home(&self.target_path, user);
        let full_cmd = format!("cd {} && {}", shell_quote(&target), cmd);
        let config = ExecConfig {
            cmd: vec!["/bin/sh".to_string(), "-c".to_string(), full_cmd],
            env: HashMap::new(),
            working_dir: None,
            user: user.map(|s| s.to_string()),
            tty: false,
            stdin: false,
            privileged: false,
        };

        let result = provider.exec(container_id, &config).await?;
        if result.exit_code != 0 {
            tracing::warn!(
                "Dotfiles install command failed with exit code {}: {}",
                result.exit_code,
                result.output
            );
        }

        Ok(())
    }

    /// Try to run default install scripts
    async fn run_default_install(
        &self,
        provider: &dyn ContainerProvider,
        container_id: &ContainerId,
        user: Option<&str>,
    ) -> Result<()> {
        let install_scripts = ["install.sh", "install", "bootstrap.sh", "bootstrap", "setup.sh"];
        let target = expand_home(&self.target_path, user);

        for script in &install_scripts {
            let check_cmd = format!("test -x {}/{}", shell_quote(&target), script);
            let config = ExecConfig {
                cmd: vec!["/bin/sh".to_string(), "-c".to_string(), check_cmd],
                env: HashMap::new(),
                working_dir: None,
                user: user.map(|s| s.to_string()),
                tty: false,
                stdin: false,
                privileged: false,
            };

            let result = provider.exec(container_id, &config).await?;
            if result.exit_code == 0 {
                tracing::info!("Running dotfiles install script: {}", script);

                // Use cd in the shell command since podman's --workdir doesn't expand ~
                let run_cmd = format!("cd {} && ./{}", shell_quote(&target), script);
                let config = ExecConfig {
                    cmd: vec!["/bin/sh".to_string(), "-c".to_string(), run_cmd],
                    env: HashMap::new(),
                    working_dir: None,
                    user: user.map(|s| s.to_string()),
                    tty: false,
                    stdin: false,
                    privileged: false,
                };

                let result = provider.exec(container_id, &config).await?;
                if result.exit_code != 0 {
                    tracing::warn!(
                        "Dotfiles install script {} failed with exit code {}: {}",
                        script,
                        result.exit_code,
                        result.output
                    );
                }

                return Ok(());
            }
        }

        tracing::debug!("No default install script found in dotfiles");
        Ok(())
    }

    /// Symlink standard dotfiles from the dotfiles directory
    async fn symlink_dotfiles(
        &self,
        provider: &dyn ContainerProvider,
        container_id: &ContainerId,
        user: Option<&str>,
    ) -> Result<()> {
        let dotfiles = [
            ".bashrc",
            ".bash_profile",
            ".zshrc",
            ".zprofile",
            ".gitconfig",
            ".vimrc",
            ".tmux.conf",
            ".inputrc",
        ];

        let target = expand_home(&self.target_path, user);
        let home = expand_home("~", user);

        for dotfile in &dotfiles {
            let src = shell_quote(&format!("{}/{}", target, dotfile));
            let dest = shell_quote(&format!("{}/{}", home, dotfile));

            let cmd = format!(
                "if [ -f {} ] && [ ! -L {} ]; then ln -sf {} {}; fi",
                src, dest, src, dest
            );

            let config = ExecConfig {
                cmd: vec!["/bin/sh".to_string(), "-c".to_string(), cmd],
                env: HashMap::new(),
                working_dir: None,
                user: user.map(|s| s.to_string()),
                tty: false,
                stdin: false,
                privileged: false,
            };

            // Ignore errors for individual symlinks
            let _ = provider.exec(container_id, &config).await;
        }

        Ok(())
    }
}

/// Helper to send progress messages
fn send_progress(progress: Option<&tokio::sync::mpsc::UnboundedSender<String>>, msg: &str) {
    if let Some(tx) = progress {
        let _ = tx.send(msg.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dotfiles_source_from_config() {
        let mut config = GlobalConfig::default();
        config.defaults.dotfiles_repo = Some("https://github.com/user/dotfiles".to_string());

        let manager = DotfilesManager::from_global_config(&config);
        assert!(manager.is_configured());
        assert!(matches!(manager.config, DotfilesSource::Repository(_)));
    }

    #[test]
    fn test_no_dotfiles() {
        let config = GlobalConfig::default();
        let manager = DotfilesManager::from_global_config(&config);
        assert!(!manager.is_configured());
    }

    #[test]
    fn test_expand_home_root() {
        assert_eq!(expand_home("~/foo", Some("root")), "/root/foo");
        assert_eq!(expand_home("~/foo", None), "/root/foo");
    }

    #[test]
    fn test_expand_home_user() {
        assert_eq!(expand_home("~/foo", Some("alice")), "/home/alice/foo");
    }

    #[test]
    fn test_expand_home_no_tilde() {
        assert_eq!(expand_home("/absolute/path", Some("user")), "/absolute/path");
    }

    #[test]
    fn test_expand_home_tilde_subpath() {
        assert_eq!(expand_home("~/.config/nvim", Some("bob")), "/home/bob/.config/nvim");
    }

    #[test]
    fn test_dotfiles_source_priority() {
        // Devcontainer config should take priority over global config
        let mut global = GlobalConfig::default();
        global.defaults.dotfiles_repo = Some("https://github.com/global/dots".to_string());

        let dc_config = DotfilesConfig {
            repository: Some("https://github.com/local/dots".to_string()),
            local_path: None,
            install_command: Some("./install.sh".to_string()),
            target_path: Some("~/.mydots".to_string()),
        };

        let manager = DotfilesManager::from_devcontainer_config(&dc_config, &global);
        assert!(manager.is_configured());
        assert!(matches!(manager.config, DotfilesSource::Repository(ref url) if url.contains("local/dots")));
        assert_eq!(manager.target_path, "~/.mydots");
        assert_eq!(manager.install_command.as_deref(), Some("./install.sh"));
    }

    #[tokio::test]
    async fn test_inject_from_repo_host_bad_url() {
        use crate::test_support::MockProvider;
        use devc_provider::ProviderType;

        let provider = MockProvider::new(ProviderType::Docker);
        let container_id = ContainerId::new("test_container");

        let manager = DotfilesManager {
            config: DotfilesSource::Repository("https://invalid.example.com/no-such-repo.git".to_string()),
            target_path: "~/.dotfiles".to_string(),
            install_command: None,
        };

        let result = manager
            .inject_from_repo_host(&provider, &container_id, "https://invalid.example.com/no-such-repo.git", None)
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, CoreError::DotfilesError(ref msg) if msg.contains("clone failed")),
            "Expected DotfilesError about clone failure, got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn test_inject_from_repo_host_copies_into_container() {
        use crate::test_support::{MockCall, MockProvider};
        use devc_provider::ProviderType;
        use std::process::Command;

        let provider = MockProvider::new(ProviderType::Docker);
        let container_id = ContainerId::new("test_container");

        // Create a local bare git repo to clone from
        let bare_dir = tempfile::tempdir().unwrap();
        let bare_path = bare_dir.path().join("dotfiles.git");
        Command::new("git")
            .args(["init", "--bare"])
            .arg(&bare_path)
            .output()
            .expect("git must be available to run tests");

        // Create a temporary repo, add a file, and push to the bare repo
        let work_dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["clone"])
            .arg(&bare_path)
            .arg(work_dir.path().join("work"))
            .output()
            .unwrap();
        let work_path = work_dir.path().join("work");
        std::fs::write(work_path.join(".bashrc"), "# test dotfile\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&work_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&work_path)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test")
            .output()
            .unwrap();
        Command::new("git")
            .args(["push"])
            .current_dir(&work_path)
            .output()
            .unwrap();

        let url = bare_path.to_str().unwrap();

        let manager = DotfilesManager {
            config: DotfilesSource::Repository(url.to_string()),
            target_path: "~/.dotfiles".to_string(),
            install_command: None,
        };

        let result = manager
            .inject_from_repo_host(&provider, &container_id, url, Some("alice"))
            .await;

        assert!(result.is_ok(), "inject_from_repo_host failed: {:?}", result);

        let calls = provider.get_calls();
        // Should have an Exec (mkdir) and a CopyInto call
        assert!(
            calls.iter().any(|c| matches!(c, MockCall::CopyInto { .. })),
            "Expected CopyInto call, got: {:?}",
            calls
        );
    }

    #[tokio::test]
    async fn test_inject_from_repo_falls_back_on_container_failure() {
        use crate::test_support::{MockCall, MockProvider};
        use devc_provider::ProviderType;
        use std::process::Command;

        // Set up mock provider where in-container exec fails (exit code 127 = command not found)
        let provider = MockProvider::new(ProviderType::Docker);
        *provider.exec_exit_code.lock().unwrap() = 127;
        *provider.exec_output.lock().unwrap() = "/bin/sh: git: not found".to_string();

        let container_id = ContainerId::new("test_container");

        // Create a local bare git repo
        let bare_dir = tempfile::tempdir().unwrap();
        let bare_path = bare_dir.path().join("dotfiles.git");
        Command::new("git")
            .args(["init", "--bare"])
            .arg(&bare_path)
            .output()
            .unwrap();

        let work_dir = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["clone"])
            .arg(&bare_path)
            .arg(work_dir.path().join("work"))
            .output()
            .unwrap();
        let work_path = work_dir.path().join("work");
        std::fs::write(work_path.join(".zshrc"), "# zsh dotfile\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&work_path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&work_path)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@test")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@test")
            .output()
            .unwrap();
        Command::new("git")
            .args(["push"])
            .current_dir(&work_path)
            .output()
            .unwrap();

        let url = bare_path.to_str().unwrap();

        let result = DotfilesManager {
            config: DotfilesSource::Repository(url.to_string()),
            target_path: "~/.dotfiles".to_string(),
            install_command: None,
        }
        .inject_from_repo(&provider, &container_id, url, Some("bob"), None)
        .await;

        assert!(result.is_ok(), "inject_from_repo should succeed via fallback: {:?}", result);

        let calls = provider.get_calls();
        // First call: the in-container exec (git clone) that failed
        assert!(
            calls.iter().any(|c| matches!(c, MockCall::Exec { .. })),
            "Expected in-container Exec call"
        );
        // Then: CopyInto from the host-side fallback
        assert!(
            calls.iter().any(|c| matches!(c, MockCall::CopyInto { .. })),
            "Expected CopyInto from host-side fallback, got: {:?}",
            calls
        );
    }
}
