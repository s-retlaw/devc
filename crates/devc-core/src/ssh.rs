//! SSH setup for proper TTY/terminal resize support
//!
//! Uses dropbear in inetd mode over stdio to work around podman exec's
//! SIGWINCH propagation issues (podman#3946).
//!
//! # Security
//!
//! - SSH keys are stored in the user's data directory with 0600 permissions
//! - Container host keys are ephemeral (generated per-container)
//! - Host key verification is disabled for container connections since they are
//!   over stdio (not network), but this means MITM protection relies on the
//!   security of the container runtime's exec mechanism

use crate::{CoreError, Result};
use devc_config::GlobalConfig;
use devc_provider::{ContainerId, ContainerProvider, ExecConfig};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

/// Manages SSH keys and container SSH setup
pub struct SshManager {
    /// Path to the private key
    key_path: PathBuf,
    /// Path to the public key
    pub_key_path: PathBuf,
}

impl SshManager {
    /// Create a new SshManager with default key paths
    pub fn new() -> Result<Self> {
        let data_dir = GlobalConfig::data_dir()?;
        let ssh_dir = data_dir.join("ssh");

        Ok(Self {
            key_path: ssh_dir.join("id_ed25519"),
            pub_key_path: ssh_dir.join("id_ed25519.pub"),
        })
    }

    /// Create SshManager with custom key path
    ///
    /// The public key path will be the same path with ".pub" appended
    pub fn with_key_path(key_path: PathBuf) -> Self {
        // Append .pub to the full path (don't replace extension)
        let mut pub_key_path: OsString = key_path.clone().into();
        pub_key_path.push(".pub");
        let pub_key_path = PathBuf::from(pub_key_path);

        Self {
            key_path,
            pub_key_path,
        }
    }

    /// Get the private key path
    pub fn key_path(&self) -> &PathBuf {
        &self.key_path
    }

    /// Get the public key path
    pub fn pub_key_path(&self) -> &PathBuf {
        &self.pub_key_path
    }

    /// Ensure SSH keypair exists, generating if necessary
    pub fn ensure_keys_exist(&self) -> Result<()> {
        if self.key_path.exists() && self.pub_key_path.exists() {
            tracing::debug!("SSH keys already exist at {:?}", self.key_path);
            return Ok(());
        }

        tracing::info!("Generating SSH keypair at {:?}", self.key_path);

        // Ensure parent directory exists
        if let Some(parent) = self.key_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let key_path_str = self
            .key_path
            .to_str()
            .ok_or_else(|| CoreError::SshKeygenError("Key path contains invalid UTF-8".into()))?;

        // Generate ed25519 keypair using ssh-keygen
        let output = Command::new("ssh-keygen")
            .args([
                "-t",
                "ed25519",
                "-f",
                key_path_str,
                "-N",
                "", // Empty passphrase
                "-C",
                "devc-container-access",
            ])
            .output()
            .map_err(|e| CoreError::SshKeygenError(format!("Failed to run ssh-keygen: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CoreError::SshKeygenError(format!(
                "ssh-keygen failed: {}",
                stderr
            )));
        }

        // Set proper permissions on private key
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&self.key_path)?.permissions();
            perms.set_mode(0o600);
            std::fs::set_permissions(&self.key_path, perms)?;
        }

        tracing::info!("Generated SSH keypair successfully");
        Ok(())
    }

    /// Setup SSH access in a container
    ///
    /// This:
    /// 1. Installs dropbear if not present (should be pre-installed via enhanced build)
    /// 2. Generates host key
    /// 3. Copies public key to authorized_keys
    pub async fn setup_container(
        &self,
        provider: &dyn ContainerProvider,
        container_id: &ContainerId,
        user: Option<&str>,
    ) -> Result<()> {
        let user = user.unwrap_or("root");

        // Validate username to prevent command injection
        Self::validate_username(user)?;

        tracing::info!(
            "Setting up SSH access in container {} for user {}",
            container_id.0,
            user
        );

        // Read and validate public key
        let pub_key = std::fs::read_to_string(&self.pub_key_path)
            .map_err(|e| CoreError::SshSetupError(format!("Failed to read public key: {}", e)))?;

        // Validate that the key looks like an SSH public key
        Self::validate_ssh_public_key(&pub_key)?;

        // Check if dropbear and socat are installed (should be, from enhanced build)
        let check_script =
            "command -v dropbear >/dev/null 2>&1 && command -v socat >/dev/null 2>&1";
        let tools_installed = self
            .exec_in_container(provider, container_id, check_script, Some("root"))
            .await
            .is_ok();

        if tools_installed {
            tracing::debug!("dropbear and socat already installed (from image build)");
        } else {
            // Fallback: install at runtime if somehow not in image
            // This can happen with ssh_enabled=false during build, then enabled later
            tracing::warn!(
                "SSH tools not found in image, installing at runtime (slower). \
                 Consider rebuilding with 'devc build' for faster startup."
            );

            let install_script = r#"
set -e
if command -v apt-get >/dev/null 2>&1; then
    apt-get update -qq && apt-get install -y -qq dropbear socat >/dev/null 2>&1
elif command -v dnf >/dev/null 2>&1; then
    dnf install -y -q dropbear socat >/dev/null 2>&1
elif command -v yum >/dev/null 2>&1; then
    yum install -y -q dropbear socat >/dev/null 2>&1
elif command -v apk >/dev/null 2>&1; then
    apk add --quiet dropbear socat >/dev/null 2>&1
elif command -v pacman >/dev/null 2>&1; then
    pacman -Sy --noconfirm --quiet dropbear socat >/dev/null 2>&1
elif command -v zypper >/dev/null 2>&1; then
    zypper -q install -y dropbear socat >/dev/null 2>&1
else
    echo "No supported package manager found" >&2
    exit 1
fi
"#;

            self.exec_in_container(provider, container_id, install_script, Some("root"))
                .await
                .map_err(|e| {
                    CoreError::SshSetupError(format!("Failed to install SSH tools: {}", e))
                })?;
        }

        // Generate dropbear host key and start daemon
        // We run dropbear as a daemon on 127.0.0.1:2222 (internal only)
        // because inetd mode doesn't work over pipes from podman exec
        let hostkey_script = r#"
set -e
mkdir -p /etc/dropbear
if [ ! -f /etc/dropbear/dropbear_ed25519_host_key ]; then
    dropbearkey -t ed25519 -f /etc/dropbear/dropbear_ed25519_host_key >/dev/null 2>&1
fi
# Start dropbear daemon if not already running
if ! pgrep -x dropbear >/dev/null 2>&1; then
    /usr/sbin/dropbear -s -r /etc/dropbear/dropbear_ed25519_host_key -p 127.0.0.1:2222 2>/dev/null
fi
"#;

        self.exec_in_container(provider, container_id, hostkey_script, Some("root"))
            .await
            .map_err(|e| CoreError::SshSetupError(format!("Failed to setup dropbear: {}", e)))?;

        // Setup authorized_keys for the user
        // Use base64 encoding to safely pass the key content without shell escaping issues
        let home_dir = if user == "root" {
            "/root".to_string()
        } else {
            format!("/home/{}", user)
        };

        // Base64 encode the key to avoid any shell injection issues
        use std::io::Write;
        let mut encoder =
            base64::write::EncoderStringWriter::new(&base64::engine::general_purpose::STANDARD);
        encoder.write_all(pub_key.trim().as_bytes()).unwrap();
        let pub_key_b64 = encoder.into_inner();

        // The script:
        // 1. Creates .ssh directory with correct permissions
        // 2. Decodes the base64 key
        // 3. Only adds it if not already present (idempotent)
        // 4. Sets correct ownership
        let auth_key_script = format!(
            r#"
set -e
mkdir -p {home}/.ssh
chmod 700 {home}/.ssh
touch {home}/.ssh/authorized_keys
chmod 600 {home}/.ssh/authorized_keys
KEY=$(echo '{pub_key_b64}' | base64 -d)
if ! grep -qF "$KEY" {home}/.ssh/authorized_keys 2>/dev/null; then
    echo "$KEY" >> {home}/.ssh/authorized_keys
fi
chown -R {user}:{user} {home}/.ssh 2>/dev/null || true
"#,
            home = home_dir,
            pub_key_b64 = pub_key_b64,
            user = user
        );

        self.exec_in_container(provider, container_id, &auth_key_script, Some("root"))
            .await
            .map_err(|e| {
                CoreError::SshSetupError(format!("Failed to setup authorized_keys: {}", e))
            })?;

        tracing::info!("SSH setup complete for container {}", container_id.0);
        Ok(())
    }

    /// Validate a username to prevent command injection
    fn validate_username(user: &str) -> Result<()> {
        // Standard Unix username: starts with lowercase letter or underscore,
        // followed by lowercase letters, digits, underscores, or hyphens
        // Maximum length is typically 32 characters
        if user.is_empty() || user.len() > 32 {
            return Err(CoreError::SshSetupError(format!(
                "Invalid username length: {}",
                user.len()
            )));
        }

        let first_char = user.chars().next().expect("validated non-empty above");
        if !first_char.is_ascii_lowercase() && first_char != '_' {
            return Err(CoreError::SshSetupError(format!(
                "Invalid username '{}': must start with lowercase letter or underscore",
                user
            )));
        }

        if !user
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return Err(CoreError::SshSetupError(format!(
                "Invalid username '{}': contains invalid characters",
                user
            )));
        }

        Ok(())
    }

    /// Validate that a string looks like a valid SSH public key
    fn validate_ssh_public_key(key: &str) -> Result<()> {
        let key = key.trim();

        // SSH public keys start with the key type
        let valid_prefixes = [
            "ssh-ed25519",
            "ssh-rsa",
            "ecdsa-sha2-nistp256",
            "ecdsa-sha2-nistp384",
            "ecdsa-sha2-nistp521",
            "sk-ssh-ed25519@openssh.com",
            "sk-ecdsa-sha2-nistp256@openssh.com",
        ];

        if !valid_prefixes.iter().any(|p| key.starts_with(p)) {
            return Err(CoreError::SshSetupError(
                "Invalid SSH public key format: must start with a valid key type".into(),
            ));
        }

        // Key should have at least 2 space-separated parts (type and key data)
        let parts: Vec<&str> = key.split_whitespace().collect();
        if parts.len() < 2 {
            return Err(CoreError::SshSetupError(
                "Invalid SSH public key format: missing key data".into(),
            ));
        }

        // The key data should be valid base64
        if base64::Engine::decode(&base64::engine::general_purpose::STANDARD, parts[1]).is_err() {
            return Err(CoreError::SshSetupError(
                "Invalid SSH public key format: key data is not valid base64".into(),
            ));
        }

        Ok(())
    }

    /// Check if SSH is ready in a container (dropbear installed)
    pub async fn is_ssh_ready(
        &self,
        provider: &dyn ContainerProvider,
        container_id: &ContainerId,
    ) -> bool {
        let check_script = "command -v dropbear >/dev/null 2>&1 && test -f /etc/dropbear/dropbear_ed25519_host_key";

        self.exec_in_container(provider, container_id, check_script, Some("root"))
            .await
            .is_ok()
    }

    /// Execute a script in the container
    async fn exec_in_container(
        &self,
        provider: &dyn ContainerProvider,
        container_id: &ContainerId,
        script: &str,
        user: Option<&str>,
    ) -> Result<()> {
        let config = ExecConfig {
            cmd: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
            env: HashMap::new(),
            working_dir: None,
            user: user.map(|s| s.to_string()),
            tty: false,
            stdin: false,
            privileged: false,
        };

        let result = provider.exec(container_id, &config).await?;

        if result.exit_code != 0 {
            return Err(CoreError::SshSetupError(format!(
                "Command exited with code {}: {}",
                result.exit_code, result.output
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_validate_username_length_boundary() {
        // 32 chars should be ok
        let name_32 = "a".repeat(32);
        assert!(SshManager::validate_username(&name_32).is_ok());

        // 33 chars should fail
        let name_33 = "a".repeat(33);
        assert!(SshManager::validate_username(&name_33).is_err());
    }

    #[test]
    fn test_validate_ssh_key_rsa() {
        // A valid RSA key (truncated but base64-valid)
        let key = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQC7 test@host";
        assert!(SshManager::validate_ssh_public_key(key).is_ok());
    }

    #[test]
    fn test_validate_ssh_key_ecdsa() {
        // A valid ecdsa-sha2-nistp256 key prefix with valid base64
        let key = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTY= test@host";
        assert!(SshManager::validate_ssh_public_key(key).is_ok());
    }

    #[test]
    fn test_validate_ssh_key_invalid_base64() {
        let key = "ssh-ed25519 !!!not-base64!!! test@host";
        assert!(SshManager::validate_ssh_public_key(key).is_err());
    }

    #[test]
    fn test_ssh_manager_paths() {
        let manager = SshManager::with_key_path(PathBuf::from("/tmp/test_key"));
        assert_eq!(manager.key_path(), &PathBuf::from("/tmp/test_key"));
        assert_eq!(manager.pub_key_path(), &PathBuf::from("/tmp/test_key.pub"));
    }

    #[test]
    fn test_ssh_manager_paths_with_extension() {
        // Test that .pub is appended, not replacing extension
        let manager = SshManager::with_key_path(PathBuf::from("/tmp/test.key"));
        assert_eq!(manager.pub_key_path(), &PathBuf::from("/tmp/test.key.pub"));
    }

    #[test]
    fn test_validate_username_valid() {
        assert!(SshManager::validate_username("root").is_ok());
        assert!(SshManager::validate_username("user").is_ok());
        assert!(SshManager::validate_username("user123").is_ok());
        assert!(SshManager::validate_username("user_name").is_ok());
        assert!(SshManager::validate_username("user-name").is_ok());
        assert!(SshManager::validate_username("_user").is_ok());
    }

    #[test]
    fn test_validate_username_invalid() {
        assert!(SshManager::validate_username("").is_err());
        assert!(SshManager::validate_username("User").is_err()); // uppercase
        assert!(SshManager::validate_username("123user").is_err()); // starts with digit
        assert!(SshManager::validate_username("user;rm").is_err()); // shell metachar
        assert!(SshManager::validate_username("user name").is_err()); // space
        assert!(SshManager::validate_username("../etc").is_err()); // path traversal
    }

    #[test]
    fn test_validate_ssh_public_key_valid() {
        let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl test@example.com";
        assert!(SshManager::validate_ssh_public_key(key).is_ok());
    }

    #[test]
    fn test_validate_ssh_public_key_invalid() {
        // Not an SSH key
        assert!(SshManager::validate_ssh_public_key("not a key").is_err());
        // Shell injection attempt
        assert!(SshManager::validate_ssh_public_key("'; rm -rf / #").is_err());
        // Missing key data
        assert!(SshManager::validate_ssh_public_key("ssh-ed25519").is_err());
    }
}
