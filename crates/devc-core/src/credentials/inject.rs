//! Container-side credential injection and refresh
//!
//! Installs chaining credential helper scripts inside the container and
//! writes resolved credentials to a tmpfs mount at `/run/devc-creds/`.

use crate::credentials::host;
use crate::{CoreError, Result};
use devc_config::GlobalConfig;
use devc_provider::{ContainerId, ContainerProvider, ExecConfig};
use std::collections::HashMap;

/// The tmpfs mount path inside the container for credential cache
pub const CREDS_TMPFS_PATH: &str = "/run/devc-creds";

/// Docker credential helper script template.
///
/// `{{original}}` is replaced with the original credsStore value (or empty).
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

# Extract auth for this registry. Pass registry via env var to avoid injection.
AUTH=$(DEVC_REGISTRY="$REGISTRY" python3 -c "
import sys, json, os
registry = os.environ['DEVC_REGISTRY']
try:
    config = json.load(sys.stdin)
    auth = config.get('auths', {}).get(registry, {}).get('auth', '')
    if not auth:
        sys.exit(1)
    import base64
    decoded = base64.b64decode(auth).decode()
    user, secret = decoded.split(':', 1)
    json.dump({'ServerURL': registry, 'Username': user, 'Secret': secret}, sys.stdout)
except:
    sys.exit(1)
" < "$CONFIG" 2>/dev/null)

if [ $? -eq 0 ] && [ -n "$AUTH" ]; then
    echo "$AUTH"
    exit 0
fi
exit 1
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
/// Call this before every shell/exec.
pub async fn setup_credentials(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    global_config: &GlobalConfig,
    user: Option<&str>,
) -> Result<()> {
    if !global_config.credentials.docker && !global_config.credentials.git {
        return Ok(());
    }

    tracing::info!(
        "Setting up credential forwarding for container {}",
        container_id.0
    );

    // Inject helpers (idempotent — skips if already injected)
    inject_helpers(provider, container_id, user).await?;

    // Refresh credential cache in tmpfs
    refresh_credentials(provider, container_id, global_config).await?;

    Ok(())
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
async fn inject_helpers(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    user: Option<&str>,
) -> Result<()> {
    // 1. Read the container's current credsStore
    let current_creds_store = read_container_creds_store(provider, container_id, user).await;

    if current_creds_store.as_deref() == Some("devc") {
        tracing::debug!("Credential helpers already injected, skipping");
        return Ok(());
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

    Ok(())
}

/// Refresh credential cache on the tmpfs mount.
///
/// Resolves Docker and Git credentials on the host, then writes them into
/// the container's `/run/devc-creds/` directory.
async fn refresh_credentials(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    global_config: &GlobalConfig,
) -> Result<()> {
    // Ensure the tmpfs directory exists (it should from the mount, but just in case)
    exec_script(
        provider,
        container_id,
        &format!("mkdir -p {}", CREDS_TMPFS_PATH),
        Some("root"),
    )
    .await
    .ok();

    // Resolve and write Docker credentials
    if global_config.credentials.docker {
        let docker_auths = host::resolve_docker_credentials().await;
        if docker_auths.is_empty() {
            tracing::debug!("No Docker credentials found on host, skipping");
        } else {
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
                docker_auths.len()
            );
        }
    }

    // Resolve and write Git credentials
    if global_config.credentials.git {
        let git_creds = host::resolve_git_credentials().await;
        if git_creds.is_empty() {
            tracing::debug!("No Git credentials found on host, skipping");
        } else {
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
                git_creds.len()
            );
        }
    }

    Ok(())
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
    // Use $HOME so it works for any user
    let script = r#"
mkdir -p "$HOME/.docker"
if [ -f "$HOME/.docker/config.json" ]; then
    python3 -c "
import json, os, sys
home = os.environ['HOME']
path = home + '/.docker/config.json'
try:
    with open(path) as f:
        config = json.load(f)
except:
    config = {}
config['credsStore'] = 'devc'
with open(path, 'w') as f:
    json.dump(config, f, indent=2)
" 2>/dev/null || echo '{"credsStore":"devc"}' > "$HOME/.docker/config.json"
else
    echo '{"credsStore":"devc"}' > "$HOME/.docker/config.json"
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

/// Execute a script in the container
async fn exec_script(
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
    let config = ExecConfig {
        cmd: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
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

        let result = setup_credentials(&provider, &container_id, &config, None).await;
        assert!(result.is_ok());
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

        let result = setup_credentials(&provider, &container_id, &config, None).await;
        assert!(result.is_ok());

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
