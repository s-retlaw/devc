//! Container-side credential injection and refresh
//!
//! Installs chaining credential helper scripts inside the container and
//! writes resolved credentials to a tmpfs mount at `/run/devc-creds/`.

use crate::credentials::host;
use crate::{CoreError, Result};
use devc_config::GlobalConfig;
use devc_provider::{ContainerId, ContainerProvider, ExecConfig};
use std::collections::HashMap;
use std::path::Path;

/// The tmpfs mount path inside the container for credential cache
pub const CREDS_TMPFS_PATH: &str = "/run/devc-creds";

/// Result of credential setup, for user-visible reporting
#[derive(Debug, Default, Clone, Copy)]
pub struct CredentialStatus {
    pub docker_registries: usize,
    pub git_hosts: usize,
    /// True if helper scripts were injected (first-time setup)
    pub helpers_injected: bool,
}

/// Docker credential helper script template.
///
/// `{{original}}` is replaced with the original credsStore value (or empty).
/// Uses `jq` for JSON parsing if available, otherwise falls back to pure shell.
const DOCKER_CREDENTIAL_HELPER: &str = r#"#!/bin/sh
ACTION="$1"
if [ "$ACTION" != "get" ]; then
    ORIGINAL_HELPER="{{original}}"
    [ -n "$ORIGINAL_HELPER" ] && exec docker-credential-"$ORIGINAL_HELPER" "$ACTION" 2>/dev/null
    exit 0
fi

REGISTRY=$(cat)

# 1. Try original helper (e.g. VS Code's)
ORIGINAL_HELPER="{{original}}"
if [ -n "$ORIGINAL_HELPER" ]; then
    RESULT=$(echo "$REGISTRY" | docker-credential-"$ORIGINAL_HELPER" get 2>/dev/null)
    if [ $? -eq 0 ] && [ -n "$RESULT" ]; then
        echo "$RESULT"
        exit 0
    fi
fi

# 2. Read from tmpfs cache
CONFIG="/run/devc-creds/config.json"
[ -f "$CONFIG" ] || exit 1

# Extract auth for this registry using jq or shell fallback
if command -v jq >/dev/null 2>&1; then
    AUTH_B64=$(jq -r --arg reg "$REGISTRY" '.auths[$reg].auth // empty' "$CONFIG" 2>/dev/null)
else
    AUTH_B64=$(grep -F -A3 "\"$REGISTRY\"" "$CONFIG" | grep '"auth"' | sed 's/.*"auth"[[:space:]]*:[[:space:]]*"//;s/".*//')
fi

[ -z "$AUTH_B64" ] && exit 1

DECODED=$(echo "$AUTH_B64" | base64 -d 2>/dev/null)
[ -z "$DECODED" ] && exit 1

USER="${DECODED%%:*}"
SECRET="${DECODED#*:}"

# Escape double quotes for safe JSON output
USER=$(printf '%s' "$USER" | sed 's/"/\\"/g')
SECRET=$(printf '%s' "$SECRET" | sed 's/"/\\"/g')

printf '{"ServerURL":"%s","Username":"%s","Secret":"%s"}\n' "$REGISTRY" "$USER" "$SECRET"
"#;

/// Git credential helper script template.
///
/// `{{original}}` is replaced with the full original credential.helper value.
const GIT_CREDENTIAL_HELPER: &str = r#"#!/bin/sh
ACTION="$1"
if [ "$ACTION" != "get" ]; then
    ORIGINAL_HELPER='{{original}}'
    if [ -n "$ORIGINAL_HELPER" ]; then
        # shellcheck disable=SC2086
        exec $ORIGINAL_HELPER "$ACTION" 2>/dev/null
    fi
    exit 0
fi

# Read input
INPUT=""
while IFS= read -r line; do
    [ -z "$line" ] && break
    INPUT="${INPUT}${line}
"
done

# 1. Try original helper
ORIGINAL_HELPER='{{original}}'
if [ -n "$ORIGINAL_HELPER" ]; then
    # shellcheck disable=SC2086
    RESULT=$(printf '%s\n' "$INPUT" | $ORIGINAL_HELPER get 2>/dev/null)
    if [ $? -eq 0 ] && echo "$RESULT" | grep -q "^password="; then
        printf '%s\n' "$RESULT"
        exit 0
    fi
fi

# 2. Read from tmpfs cache
CREDS_FILE="/run/devc-creds/git-credentials"
[ -f "$CREDS_FILE" ] || exit 1

HOST=$(printf '%s\n' "$INPUT" | grep "^host=" | cut -d= -f2)
PROTO=$(printf '%s\n' "$INPUT" | grep "^protocol=" | cut -d= -f2)

# git-credentials file format: https://user:pass@host
MATCH=$(grep "://" "$CREDS_FILE" | grep "@${HOST}" | head -1)
[ -z "$MATCH" ] && exit 1

USER=$(echo "$MATCH" | sed 's|.*://||;s|:.*||')
PASS=$(echo "$MATCH" | sed 's|.*://[^:]*:||;s|@.*||')

printf 'protocol=%s\nhost=%s\nusername=%s\npassword=%s\n' "$PROTO" "$HOST" "$USER" "$PASS"
"#;

/// Entry point: inject credential helpers (idempotent) and refresh credentials.
///
/// Call this before every shell/exec. Returns a status summary for user-visible
/// reporting (number of registries/hosts forwarded).
pub async fn setup_credentials(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    global_config: &GlobalConfig,
    user: Option<&str>,
    workspace_path: &Path,
) -> Result<CredentialStatus> {
    if !global_config.credentials.docker && !global_config.credentials.git {
        return Ok(CredentialStatus::default());
    }

    tracing::info!(
        "Setting up credential forwarding for container {}",
        container_id.0
    );

    // Inject helpers (idempotent — skips if already injected)
    let helpers_injected = inject_helpers(provider, container_id, user).await?;

    // Refresh credential cache in tmpfs
    let (docker_registries, git_hosts) =
        refresh_credentials(provider, container_id, global_config, workspace_path).await?;

    Ok(CredentialStatus {
        docker_registries,
        git_hosts,
        helpers_injected,
    })
}

/// Sanitize a Docker credential helper name.
///
/// Helper names like "desktop", "ecr-login", "osxkeychain", "dev-containers-<UUID>"
/// should only contain alphanumeric chars, hyphens, and underscores.
/// Strips anything else to prevent shell injection.
fn sanitize_docker_helper_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// Escape a string for safe use inside single-quoted shell strings.
///
/// Replaces `'` with `'\''` (end single quote, escaped literal quote, start single quote).
/// The result is safe to embed in `'...'` shell strings.
fn shell_escape_single_quotes(s: &str) -> String {
    s.replace('\'', "'\\''")
}

/// Inject the chaining credential helper scripts into the container.
///
/// Idempotent: if `credsStore` is already `"devc"`, skips injection.
/// If it's something else (e.g. VS Code's helper), saves it as "original"
/// in the generated scripts so credentials chain through.
///
/// Returns `true` if helpers were newly injected, `false` if already present.
async fn inject_helpers(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    user: Option<&str>,
) -> Result<bool> {
    // 1. Read the container's current credsStore
    let current_creds_store = read_container_creds_store(provider, container_id, user).await;

    if current_creds_store.as_deref() == Some("devc") {
        tracing::debug!("Credential helpers already injected, skipping");
        return Ok(false);
    }

    let original = sanitize_docker_helper_name(
        &current_creds_store.unwrap_or_default(),
    );
    tracing::info!(
        "Injecting credential helpers (original credsStore: {:?})",
        if original.is_empty() {
            "none"
        } else {
            &original
        }
    );

    // 2. Generate docker-credential-devc script
    let docker_script = DOCKER_CREDENTIAL_HELPER.replace("{{original}}", &original);
    write_script_to_container(
        provider,
        container_id,
        "/usr/local/bin/docker-credential-devc",
        &docker_script,
    )
    .await?;

    // 3. Read container's current git credential.helper
    let original_git_helper =
        read_container_git_credential_helper(provider, container_id, user).await;
    let git_original = shell_escape_single_quotes(
        &original_git_helper.unwrap_or_default(),
    );

    // 4. Generate git-credential-devc script
    let git_script = GIT_CREDENTIAL_HELPER.replace("{{original}}", &git_original);
    write_script_to_container(
        provider,
        container_id,
        "/usr/local/bin/git-credential-devc",
        &git_script,
    )
    .await?;

    // 5. Update container Docker config: set credsStore to "devc"
    set_container_creds_store(provider, container_id, user).await?;

    // 6. Update container git config: set credential.helper
    set_container_git_credential_helper(provider, container_id, user).await?;

    Ok(true)
}

/// Refresh credential cache on the tmpfs mount.
///
/// Resolves Docker and Git credentials on the host, then writes them into
/// the container's `/run/devc-creds/` directory.
///
/// Returns `(docker_registry_count, git_host_count)`.
async fn refresh_credentials(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    global_config: &GlobalConfig,
    workspace_path: &Path,
) -> Result<(usize, usize)> {
    // Ensure the tmpfs directory exists (it should from the mount, but just in case)
    exec_script(
        provider,
        container_id,
        &format!("mkdir -p {}", CREDS_TMPFS_PATH),
        Some("root"),
    )
    .await
    .ok();

    let mut docker_count = 0;
    let mut git_count = 0;

    // Resolve and write Docker credentials
    if global_config.credentials.docker {
        let docker_auths = host::resolve_docker_credentials().await;
        if docker_auths.is_empty() {
            tracing::debug!("No Docker credentials found on host, skipping");
        } else {
            docker_count = docker_auths.len();
            let config_json = host::build_docker_config_json(&docker_auths);
            write_file_to_container(
                provider,
                container_id,
                &format!("{}/config.json", CREDS_TMPFS_PATH),
                &config_json,
            )
            .await?;
            tracing::debug!(
                "Wrote Docker credentials for {} registries to tmpfs",
                docker_count
            );
        }
    }

    // Resolve and write Git credentials
    if global_config.credentials.git {
        let git_hosts = host::discover_git_hosts(workspace_path);
        let git_creds = host::resolve_git_credentials(&git_hosts).await;
        if git_creds.is_empty() {
            tracing::debug!("No Git credentials found on host, skipping");
        } else {
            git_count = git_creds.len();
            let creds_content = host::format_git_credentials(&git_creds);
            write_file_to_container(
                provider,
                container_id,
                &format!("{}/git-credentials", CREDS_TMPFS_PATH),
                &creds_content,
            )
            .await?;
            tracing::debug!(
                "Wrote Git credentials for {} hosts to tmpfs",
                git_count
            );
        }
    }

    Ok((docker_count, git_count))
}

/// Read the container's Docker config credsStore value
async fn read_container_creds_store(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    user: Option<&str>,
) -> Option<String> {
    // Use $HOME so it works for any user
    let script = r#"cat "$HOME/.docker/config.json" 2>/dev/null"#;

    let result = exec_script_with_output(provider, container_id, script, user).await?;

    let config: serde_json::Value = serde_json::from_str(&result).ok()?;
    config
        .get("credsStore")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Read the container's git credential.helper value
async fn read_container_git_credential_helper(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    user: Option<&str>,
) -> Option<String> {
    let script = "git config --global credential.helper 2>/dev/null || true";

    let result = exec_script_with_output(provider, container_id, script, user).await?;
    let trimmed = result.trim().to_string();
    if trimmed.is_empty() || trimmed == "/usr/local/bin/git-credential-devc" {
        None
    } else {
        Some(trimmed)
    }
}

/// Set credsStore to "devc" in the container's Docker config
async fn set_container_creds_store(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    user: Option<&str>,
) -> Result<()> {
    // Use $HOME so it works for any user.
    // Uses jq if available, otherwise falls back to simple file write.
    let script = r#"
mkdir -p "$HOME/.docker"
DOCKER_CFG="$HOME/.docker/config.json"
if [ -f "$DOCKER_CFG" ] && command -v jq >/dev/null 2>&1; then
    jq '.credsStore = "devc"' "$DOCKER_CFG" > "$DOCKER_CFG.tmp" && mv "$DOCKER_CFG.tmp" "$DOCKER_CFG"
elif [ -f "$DOCKER_CFG" ]; then
    # Shell fallback: if credsStore exists, replace it; otherwise add it
    if grep -q '"credsStore"' "$DOCKER_CFG" 2>/dev/null; then
        sed 's/"credsStore"[[:space:]]*:[[:space:]]*"[^"]*"/"credsStore": "devc"/' "$DOCKER_CFG" > "$DOCKER_CFG.tmp" && mv "$DOCKER_CFG.tmp" "$DOCKER_CFG"
    else
        # Insert credsStore after first opening brace (works with compact and pretty JSON, BusyBox sed)
        # Only replace the first occurrence using line-address '1'
        sed '1 s/{/{"credsStore":"devc",/' "$DOCKER_CFG" > "$DOCKER_CFG.tmp" && mv "$DOCKER_CFG.tmp" "$DOCKER_CFG"
    fi
else
    echo '{"credsStore":"devc"}' > "$DOCKER_CFG"
fi
"#;

    exec_script(provider, container_id, script, user).await?;
    Ok(())
}

/// Set credential.helper in the container's git config
async fn set_container_git_credential_helper(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    user: Option<&str>,
) -> Result<()> {
    let script = "git config --global credential.helper /usr/local/bin/git-credential-devc 2>/dev/null || true";

    exec_script(provider, container_id, script, user).await?;
    Ok(())
}

/// Write a script to the container using base64 encoding (same pattern as ssh.rs)
async fn write_script_to_container(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    path: &str,
    content: &str,
) -> Result<()> {
    let encoded = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        content.as_bytes(),
    );

    let script = format!(
        "echo '{}' | base64 -d > {} && chmod +x {}",
        encoded, path, path
    );

    exec_script(provider, container_id, &script, Some("root")).await
}

/// Write a file to the container using base64 encoding
async fn write_file_to_container(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    path: &str,
    content: &str,
) -> Result<()> {
    let encoded = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        content.as_bytes(),
    );

    // chmod 644: credential files must be readable by the container user
    // (which may not be root). Tmpfs is container-scoped and ephemeral.
    let script = format!(
        "echo '{}' | base64 -d > {} && chmod 644 {}",
        encoded, path, path
    );

    exec_script(provider, container_id, &script, Some("root")).await
}

/// Wrap a script to ensure $HOME is set correctly from /etc/passwd.
///
/// Docker/Podman exec usually sets HOME, but some runtimes or custom
/// images may not. This resolves HOME from getent/passwd for any distro
/// (Alpine, Fedora, Arch, etc.) rather than hardcoding /home/{user}.
fn wrap_with_home_resolve(script: &str) -> String {
    format!(
        r#"if [ -z "$HOME" ]; then HOME=$(getent passwd "$(whoami)" 2>/dev/null | cut -d: -f6 || echo "/root"); export HOME; fi
{}"#,
        script
    )
}

/// Execute a script in the container
async fn exec_script(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    script: &str,
    user: Option<&str>,
) -> Result<()> {
    let wrapped = wrap_with_home_resolve(script);
    let config = ExecConfig {
        cmd: vec!["/bin/sh".to_string(), "-c".to_string(), wrapped],
        env: HashMap::new(),
        working_dir: None,
        user: user.map(|s| s.to_string()),
        tty: false,
        stdin: false,
        privileged: false,
    };

    let result = provider.exec(container_id, &config).await.map_err(|e| {
        CoreError::CredentialError(format!("Failed to exec in container: {}", e))
    })?;

    if result.exit_code != 0 {
        return Err(CoreError::CredentialError(format!(
            "Script exited with code {}: {}",
            result.exit_code, result.output
        )));
    }

    Ok(())
}

/// Execute a script and return its stdout
async fn exec_script_with_output(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    script: &str,
    user: Option<&str>,
) -> Option<String> {
    let wrapped = wrap_with_home_resolve(script);
    let config = ExecConfig {
        cmd: vec!["/bin/sh".to_string(), "-c".to_string(), wrapped],
        env: HashMap::new(),
        working_dir: None,
        user: user.map(|s| s.to_string()),
        tty: false,
        stdin: false,
        privileged: false,
    };

    let result = provider.exec(container_id, &config).await.ok()?;
    if result.exit_code != 0 {
        return None;
    }

    Some(result.output)
}

/// Generate the Docker credential helper script (for testing)
#[cfg(test)]
pub fn generate_docker_helper_script(original: &str) -> String {
    DOCKER_CREDENTIAL_HELPER.replace("{{original}}", original)
}

/// Generate the Git credential helper script (for testing)
#[cfg(test)]
pub fn generate_git_helper_script(original: &str) -> String {
    GIT_CREDENTIAL_HELPER.replace("{{original}}", original)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_docker_helper_script_no_original() {
        let script = generate_docker_helper_script("");
        assert!(script.contains(r#"ORIGINAL_HELPER="""#));
        assert!(script.contains("/run/devc-creds/config.json"));
        assert!(script.contains("docker-credential-"));
        // Should not require python3
        assert!(!script.contains("python3"));
        // Should use jq with shell fallback
        assert!(script.contains("jq"));
        assert!(script.contains("base64 -d"));
    }

    #[test]
    fn test_docker_helper_script_with_original() {
        let script = generate_docker_helper_script("desktop");
        assert!(script.contains(r#"ORIGINAL_HELPER="desktop""#));
        assert!(script.contains("docker-credential-\"$ORIGINAL_HELPER\" get"));
    }

    #[test]
    fn test_docker_helper_script_vscode_original() {
        let script = generate_docker_helper_script("dev-containers-abc123");
        assert!(script.contains(r#"ORIGINAL_HELPER="dev-containers-abc123""#));
    }

    #[test]
    fn test_git_helper_script_no_original() {
        let script = generate_git_helper_script("");
        assert!(script.contains("ORIGINAL_HELPER=''"));
        assert!(script.contains("/run/devc-creds/git-credentials"));
    }

    #[test]
    fn test_git_helper_script_with_original() {
        let script = generate_git_helper_script("/usr/local/share/vscode/git-credential-helper.sh");
        assert!(script.contains(
            "ORIGINAL_HELPER='/usr/local/share/vscode/git-credential-helper.sh'"
        ));
    }

    #[test]
    fn test_sanitize_docker_helper_name() {
        assert_eq!(sanitize_docker_helper_name("desktop"), "desktop");
        assert_eq!(sanitize_docker_helper_name("ecr-login"), "ecr-login");
        assert_eq!(sanitize_docker_helper_name("dev-containers-abc123"), "dev-containers-abc123");
        // Strips dangerous characters
        assert_eq!(sanitize_docker_helper_name("foo\"; rm -rf / #"), "foorm-rf");
        assert_eq!(sanitize_docker_helper_name(""), "");
        assert_eq!(sanitize_docker_helper_name("osxkeychain"), "osxkeychain");
    }

    #[test]
    fn test_shell_escape_single_quotes() {
        assert_eq!(shell_escape_single_quotes("simple"), "simple");
        assert_eq!(shell_escape_single_quotes("it's"), "it'\\''s");
        assert_eq!(
            shell_escape_single_quotes("/usr/local/bin/helper"),
            "/usr/local/bin/helper"
        );
    }

    #[test]
    fn test_creds_tmpfs_path() {
        assert_eq!(CREDS_TMPFS_PATH, "/run/devc-creds");
    }

    #[test]
    fn test_wrap_with_home_resolve() {
        let wrapped = wrap_with_home_resolve("echo hello");
        // Should include getent passwd lookup for $HOME
        assert!(wrapped.contains("getent passwd"));
        assert!(wrapped.contains("whoami"));
        assert!(wrapped.contains("export HOME"));
        // Original script should be at the end
        assert!(wrapped.ends_with("echo hello"));
    }

    #[test]
    fn test_wrap_preserves_script() {
        let script = r#"cat "$HOME/.docker/config.json""#;
        let wrapped = wrap_with_home_resolve(script);
        assert!(wrapped.contains(script));
    }

    #[tokio::test]
    async fn test_setup_credentials_disabled() {
        // When both docker and git credentials are disabled, setup should be a no-op
        use crate::test_support::MockProvider;
        use devc_provider::ProviderType;

        let provider = MockProvider::new(ProviderType::Docker);
        let container_id = ContainerId::new("test-container");
        let mut config = GlobalConfig::default();
        config.credentials.docker = false;
        config.credentials.git = false;

        let tmp = std::env::temp_dir();
        let result = setup_credentials(&provider, &container_id, &config, None, &tmp).await;
        assert!(result.is_ok());
        let status = result.unwrap();
        assert_eq!(status.docker_registries, 0);
        assert_eq!(status.git_hosts, 0);
        assert!(!status.helpers_injected);
        // No exec calls should have been made
        assert!(provider.get_calls().is_empty());
    }

    #[tokio::test]
    async fn test_setup_credentials_already_injected() {
        // When credsStore is already "devc", should skip injection but still refresh
        use crate::test_support::MockProvider;
        use devc_provider::ProviderType;

        let provider = MockProvider::new(ProviderType::Docker);
        // Mock exec to return credsStore: "devc" for the config read
        *provider.exec_output.lock().unwrap() = r#"{"credsStore":"devc"}"#.to_string();

        let container_id = ContainerId::new("test-container");
        let config = GlobalConfig::default();

        let tmp = std::env::temp_dir();
        let result = setup_credentials(&provider, &container_id, &config, None, &tmp).await;
        assert!(result.is_ok());
        let status = result.unwrap();
        // helpers_injected should be false since they were already there
        assert!(!status.helpers_injected);

        // Should have called exec for: read credsStore, mkdir, and credential writes
        let calls = provider.get_calls();
        assert!(!calls.is_empty());
    }

    #[tokio::test]
    async fn test_inject_helpers_writes_scripts() {
        use crate::test_support::MockProvider;
        use devc_provider::ProviderType;

        let provider = MockProvider::new(ProviderType::Docker);
        // Return empty config (no existing credsStore)
        *provider.exec_output.lock().unwrap() = String::new();

        let container_id = ContainerId::new("test-container");
        let result = inject_helpers(&provider, &container_id, None).await;
        assert!(result.is_ok());

        let calls = provider.get_calls();
        // Should have at least: read credsStore, write docker script, read git helper,
        // write git script, set credsStore, set git config
        assert!(
            calls.len() >= 4,
            "Expected at least 4 exec calls, got {}",
            calls.len()
        );
    }
}
