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

/// System-wide git config file. Writing here (as root) makes our credential
/// helper and identity visible to every user, with no dependency on `git`
/// being installed or `$HOME` being correct for the remote user.
pub const SYSTEM_GITCONFIG: &str = "/etc/gitconfig";

/// Absolute path of the injected git credential helper script.
pub const GIT_CREDENTIAL_HELPER_PATH: &str = "/usr/local/bin/git-credential-devc";

/// Absolute path of the injected Docker credential helper script.
pub const DOCKER_CREDENTIAL_HELPER_PATH: &str = "/usr/local/bin/docker-credential-devc";

/// Profile.d script that exports credential env vars (GH_TOKEN) into login
/// shells. Lifecycle commands run via `sh -lc`, so this file is sourced
/// automatically via the standard `/etc/profile` → `/etc/profile.d/*.sh` chain
/// on Ubuntu/Debian/Alpine/Fedora. Numbered `50-` to sit mid-order so earlier
/// feature scripts can observe it and later ones can override if they choose.
pub const PROFILE_D_CREDENTIALS_PATH: &str = "/etc/profile.d/50-devc-credentials.sh";

/// Content of PROFILE_D_CREDENTIALS_PATH. Reads the cached gh token (if any)
/// and exports it as `GH_TOKEN` so `gh` CLI invocations in lifecycle scripts
/// pick up the host's GitHub authentication without the caller having to
/// set the env var explicitly.
const PROFILE_D_CREDENTIALS_SCRIPT: &str = r#"# devc credential env — sourced by /etc/profile in login shells
if [ -r /run/devc-creds/gh-token ]; then
    GH_TOKEN=$(cat /run/devc-creds/gh-token 2>/dev/null)
    export GH_TOKEN
fi
"#;

/// Result of credential setup, for user-visible reporting
#[derive(Debug, Default, Clone)]
pub struct CredentialStatus {
    pub docker_registries: usize,
    pub git_hosts: usize,
    /// True if helper scripts were injected (first-time setup)
    pub helpers_injected: bool,
    /// GitHub CLI token resolved from the host, if any
    pub gh_token: Option<String>,
    /// True if git identity (user.name/email) was injected
    pub git_identity_injected: bool,
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

# 2. Read from tmpfs cache (exit 0 with no output if missing — allows anonymous pulls)
CONFIG="/run/devc-creds/config.json"
[ -f "$CONFIG" ] || exit 0

# Extract auth for this registry using jq or shell fallback
if command -v jq >/dev/null 2>&1; then
    AUTH_B64=$(jq -r --arg reg "$REGISTRY" '.auths[$reg].auth // empty' "$CONFIG" 2>/dev/null)
else
    AUTH_B64=$(grep -F -A3 "\"$REGISTRY\"" "$CONFIG" | grep '"auth"' | sed 's/.*"auth"[[:space:]]*:[[:space:]]*"//;s/".*//')
fi

[ -z "$AUTH_B64" ] && exit 0

DECODED=$(echo "$AUTH_B64" | base64 -d 2>/dev/null)
[ -z "$DECODED" ] && exit 0

USER="${DECODED%%:*}"
SECRET="${DECODED#*:}"

# Escape double quotes for safe JSON output
USER=$(printf '%s' "$USER" | sed 's/"/\\"/g')
SECRET=$(printf '%s' "$SECRET" | sed 's/"/\\"/g')

printf '{"ServerURL":"%s","Username":"%s","Secret":"%s"}\n' "$REGISTRY" "$USER" "$SECRET"
"#;

/// Git credential helper script. Emits credentials sourced from the tmpfs cache.
///
/// No chaining: this helper is registered at the system level in `/etc/gitconfig`,
/// which git supports alongside other `credential.helper` entries. Existing helpers
/// (from the base image or user-level config) are tried by git natively, so the
/// helper script itself only needs to look up its own cache.
const GIT_CREDENTIAL_HELPER: &str = r#"#!/bin/sh
[ "$1" = "get" ] || exit 0

INPUT=""
while IFS= read -r line; do
    [ -z "$line" ] && break
    INPUT="${INPUT}${line}
"
done

CREDS_FILE="/run/devc-creds/git-credentials"
[ -f "$CREDS_FILE" ] || exit 0

HOST=$(printf '%s\n' "$INPUT" | grep "^host=" | cut -d= -f2)
PROTO=$(printf '%s\n' "$INPUT" | grep "^protocol=" | cut -d= -f2)

# git-credentials file format: https://user:pass@host
MATCH=$(grep "://" "$CREDS_FILE" | grep "@${HOST}" | head -1)
[ -z "$MATCH" ] && exit 0

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
    if !global_config.credentials.docker
        && !global_config.credentials.git
        && !global_config.credentials.gh
    {
        return Ok(CredentialStatus::default());
    }

    tracing::info!(
        "Setting up credential forwarding for container {}",
        container_id.0
    );

    // Inject helpers (idempotent — skips if already injected)
    let helpers_injected = inject_helpers(provider, container_id, user).await?;

    // Inject git identity (user.name/email) from host (idempotent)
    let git_identity_injected = if global_config.credentials.git {
        inject_git_identity(provider, container_id).await
    } else {
        false
    };

    // Refresh credential cache in tmpfs
    let (docker_registries, git_hosts, gh_token) =
        refresh_credentials(provider, container_id, global_config, workspace_path).await?;

    Ok(CredentialStatus {
        docker_registries,
        git_hosts,
        helpers_injected,
        gh_token,
        git_identity_injected,
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

/// Inject credential helper scripts into the container.
///
/// Idempotent: if Docker `credsStore` is already `"devc"`, skips injection.
/// For Docker, if the image ships a different credsStore (e.g. VS Code's), that
/// name is baked into our docker-credential-devc script as a fallback chain.
/// For git we don't chain — we register our helper at the system level
/// (`/etc/gitconfig`) and rely on git's native multi-helper support.
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

    let original = sanitize_docker_helper_name(&current_creds_store.unwrap_or_default());
    tracing::info!(
        "Injecting credential helpers (original credsStore: {:?})",
        if original.is_empty() {
            "none"
        } else {
            &original
        }
    );

    // 2. Generate docker-credential-devc script (still chains through original)
    let docker_script = DOCKER_CREDENTIAL_HELPER.replace("{{original}}", &original);
    write_script_to_container(
        provider,
        container_id,
        DOCKER_CREDENTIAL_HELPER_PATH,
        &docker_script,
    )
    .await?;

    // 3. Write git-credential-devc script (no chaining — git handles it natively)
    write_script_to_container(
        provider,
        container_id,
        GIT_CREDENTIAL_HELPER_PATH,
        GIT_CREDENTIAL_HELPER,
    )
    .await?;

    // 4. Write the profile.d script that exports GH_TOKEN for login shells
    //    (lifecycle commands run via `sh -lc`, so they source this).
    write_script_to_container(
        provider,
        container_id,
        PROFILE_D_CREDENTIALS_PATH,
        PROFILE_D_CREDENTIALS_SCRIPT,
    )
    .await?;

    // 5. Update container Docker config: set credsStore to "devc"
    set_container_creds_store(provider, container_id, user).await?;

    // 6. Register git credential helper at the system level
    set_system_git_credential_helper(provider, container_id).await?;

    Ok(true)
}

/// Refresh credential cache on the tmpfs mount.
///
/// Resolves Docker, Git, and GitHub CLI credentials on the host, then writes
/// them into the container's `/run/devc-creds/` directory.
///
/// Returns `(docker_registry_count, git_host_count, gh_token)`.
async fn refresh_credentials(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    global_config: &GlobalConfig,
    workspace_path: &Path,
) -> Result<(usize, usize, Option<String>)> {
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
            tracing::debug!("Wrote Git credentials for {} hosts to tmpfs", git_count);
        }
    }

    // Resolve and write GitHub CLI token
    let gh_token = if global_config.credentials.gh {
        match host::resolve_gh_token().await {
            Some(token) => {
                write_file_to_container(
                    provider,
                    container_id,
                    &format!("{}/gh-token", CREDS_TMPFS_PATH),
                    &token,
                )
                .await?;
                tracing::debug!("Wrote GitHub CLI token to tmpfs");
                Some(token)
            }
            None => {
                tracing::debug!("No GitHub CLI token found on host, skipping");
                None
            }
        }
    } else {
        None
    };

    Ok((docker_count, git_count, gh_token))
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

/// Register our git credential helper at the *system* level (`/etc/gitconfig`).
///
/// Appends a `[credential] helper = /usr/local/bin/git-credential-devc` entry if
/// absent. Never rewrites existing entries: git supports multiple `credential.helper`
/// values and tries each in order, so pre-existing helpers (from the base image or
/// a feature) keep working and ours is a fallback. Idempotent via grep.
///
/// Runs as root (no user param): works regardless of whether `git` is installed,
/// `$HOME` is set correctly, or the remote user exists in `/etc/passwd`.
async fn set_system_git_credential_helper(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
) -> Result<()> {
    let script = format!(
        r#"
GITCONFIG="{gitconfig}"
HELPER="{helper}"
if grep -q "helper[[:space:]]*=[[:space:]]*${{HELPER}}" "$GITCONFIG" 2>/dev/null; then
    exit 0
fi
printf '\n[credential]\n\thelper = %s\n' "$HELPER" >> "$GITCONFIG"
"#,
        gitconfig = SYSTEM_GITCONFIG,
        helper = GIT_CREDENTIAL_HELPER_PATH,
    );
    exec_script(provider, container_id, &script, Some("root")).await?;
    Ok(())
}

/// Inject git identity (user.name and user.email) from the host into the container.
///
/// Inject the host's git identity (user.name / user.email) into the container's
/// system git config (`/etc/gitconfig`) as root.
///
/// Idempotent: skips if a `[user]` name is already present in `/etc/gitconfig`.
/// User-level `~/.gitconfig` (if any user happens to have one) is not consulted
/// here — that level can only override, never un-set, so writing at the system
/// level is always safe.
///
/// Writes the file directly rather than shelling out to `git config`, so it
/// works even when git isn't yet installed in the container and regardless of
/// the remote user's existence / HOME.
///
/// Returns `true` if identity was newly injected.
async fn inject_git_identity(provider: &dyn ContainerProvider, container_id: &ContainerId) -> bool {
    // Resolve identity from the host first — if there's nothing to inject, bail.
    let identity = match host::resolve_git_identity() {
        Some(id) => id,
        None => {
            tracing::debug!("No git identity found on host, skipping injection");
            return false;
        }
    };

    let escaped_name = shell_escape_single_quotes(&identity.name);
    let escaped_email = shell_escape_single_quotes(&identity.email);

    let script = format!(
        r#"
GITCONFIG="{gitconfig}"
if grep -q '^[[:space:]]*name[[:space:]]*=' "$GITCONFIG" 2>/dev/null; then
    exit 0
fi
printf '\n[user]\n\tname = %s\n\temail = %s\n' '{name}' '{email}' >> "$GITCONFIG"
"#,
        gitconfig = SYSTEM_GITCONFIG,
        name = escaped_name,
        email = escaped_email,
    );

    match exec_script(provider, container_id, &script, Some("root")).await {
        Ok(()) => {
            tracing::info!(
                "Injected git identity into /etc/gitconfig: {} <{}>",
                identity.name,
                identity.email
            );
            true
        }
        Err(e) => {
            tracing::warn!("Failed to inject git identity (non-fatal): {}", e);
            false
        }
    }
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

    let result = provider
        .exec(container_id, &config)
        .await
        .map_err(|e| CoreError::CredentialError(format!("Failed to exec in container: {}", e)))?;

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

/// Return the Git credential helper script (for testing)
#[cfg(test)]
pub fn git_helper_script() -> &'static str {
    GIT_CREDENTIAL_HELPER
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
    fn test_git_helper_script_reads_tmpfs_cache_no_chaining() {
        let script = git_helper_script();
        assert!(script.contains("/run/devc-creds/git-credentials"));
        // The script no longer chains through an original helper; git handles that
        // natively via multiple credential.helper entries in /etc/gitconfig + user config.
        assert!(!script.contains("ORIGINAL_HELPER"));
        assert!(!script.contains("{{original}}"));
    }

    #[test]
    fn test_sanitize_docker_helper_name() {
        assert_eq!(sanitize_docker_helper_name("desktop"), "desktop");
        assert_eq!(sanitize_docker_helper_name("ecr-login"), "ecr-login");
        assert_eq!(
            sanitize_docker_helper_name("dev-containers-abc123"),
            "dev-containers-abc123"
        );
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
        // When all credentials are disabled, setup should be a no-op
        use crate::test_support::MockProvider;
        use devc_provider::ProviderType;

        let provider = MockProvider::new(ProviderType::Docker);
        let container_id = ContainerId::new("test-container");
        let mut config = GlobalConfig::default();
        config.credentials.docker = false;
        config.credentials.git = false;
        config.credentials.gh = false;

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
        // Expected exec calls: read credsStore, write docker helper, write git helper,
        // set credsStore, register git helper in /etc/gitconfig.
        assert!(
            calls.len() >= 4,
            "Expected at least 4 exec calls, got {}",
            calls.len()
        );
    }

    /// Given a fresh container with no `/etc/gitconfig`, `inject_helpers` should
    /// issue an exec that appends our helper line. The MockProvider's default
    /// exec_output is empty so `grep -q` returns non-zero, triggering the append.
    #[tokio::test]
    async fn test_set_system_git_credential_helper_writes_gitconfig_when_missing() {
        use crate::test_support::{MockCall, MockProvider};
        use devc_provider::ProviderType;

        let provider = MockProvider::new(ProviderType::Docker);
        let container_id = ContainerId::new("test-container");

        set_system_git_credential_helper(&provider, &container_id)
            .await
            .expect("system git helper registration");

        let calls = provider.get_calls();
        let ran_as_root_with_gitconfig_write = calls.iter().any(|c| {
            matches!(
                c,
                MockCall::Exec { user, cmd, .. }
                    if user.as_deref() == Some("root")
                    && cmd.iter().any(|s|
                        s.contains("/etc/gitconfig")
                        && s.contains("/usr/local/bin/git-credential-devc"))
            )
        });
        assert!(
            ran_as_root_with_gitconfig_write,
            "Expected an exec as root that references /etc/gitconfig and the helper path; got: {:?}",
            calls
        );
    }

    /// When `/etc/gitconfig` already has our helper line, the script's grep
    /// short-circuits with `exit 0` before any append. Since the exec is a
    /// single script invocation we can't assert "no append happened" directly —
    /// instead we verify only ONE exec call is made (the grep+append script
    /// runs atomically), matching the single-exec behavior.
    #[tokio::test]
    async fn test_set_system_git_credential_helper_single_exec() {
        use crate::test_support::{MockCall, MockProvider};
        use devc_provider::ProviderType;

        let provider = MockProvider::new(ProviderType::Docker);
        let container_id = ContainerId::new("test-container");

        set_system_git_credential_helper(&provider, &container_id)
            .await
            .expect("system git helper registration");

        let exec_calls: Vec<_> = provider
            .get_calls()
            .into_iter()
            .filter(|c| matches!(c, MockCall::Exec { .. }))
            .collect();
        assert_eq!(
            exec_calls.len(),
            1,
            "Expected a single atomic exec (grep+append in one script); got {} calls",
            exec_calls.len()
        );
    }

    /// `inject_helpers` must not depend on `git` being installed in the container.
    /// Today's implementation registers the helper by writing `/etc/gitconfig` as
    /// root — plain shell, no `git` CLI. This test verifies that the recorded
    /// exec calls don't invoke `git` anywhere.
    #[tokio::test]
    async fn test_inject_helpers_does_not_invoke_git_binary() {
        use crate::test_support::{MockCall, MockProvider};
        use devc_provider::ProviderType;

        let provider = MockProvider::new(ProviderType::Docker);
        *provider.exec_output.lock().unwrap() = String::new();

        let container_id = ContainerId::new("test-container");
        inject_helpers(&provider, &container_id, None)
            .await
            .expect("inject helpers");

        let any_invokes_git = provider.get_calls().iter().any(|c| {
            matches!(
                c,
                MockCall::Exec { cmd, .. }
                    if cmd.iter().any(|s| {
                        // Match `git ` followed by a subcommand (config/help/etc.) —
                        // don't false-positive on `/usr/local/bin/git-credential-devc`
                        // or the /etc/gitconfig file path.
                        s.contains("git config")
                            || s.contains("git init")
                            || s.contains("git clone")
                    })
            )
        });
        assert!(
            !any_invokes_git,
            "inject_helpers should not shell out to the git binary \
             (it must work even when git isn't installed); calls: {:?}",
            provider.get_calls()
        );
    }

    /// `inject_helpers` must write `/etc/profile.d/50-devc-credentials.sh` so
    /// that lifecycle scripts (run via `sh -lc`) automatically export `GH_TOKEN`
    /// from the cached credential file. Otherwise `gh auth` calls in an
    /// `onCreateCommand` / `postCreateCommand` see no token and fail.
    #[tokio::test]
    async fn test_inject_helpers_writes_profile_d_credentials_script() {
        use crate::test_support::{MockCall, MockProvider};
        use devc_provider::ProviderType;

        let provider = MockProvider::new(ProviderType::Docker);
        *provider.exec_output.lock().unwrap() = String::new();

        let container_id = ContainerId::new("test-container");
        inject_helpers(&provider, &container_id, None)
            .await
            .expect("inject helpers");

        // The profile.d file is written via write_script_to_container which
        // base64-encodes the content, so the exec command line references the
        // target path but not the inlined GH_TOKEN literal. Check the path.
        let wrote_profile_d = provider.get_calls().iter().any(|c| {
            matches!(
                c,
                MockCall::Exec { cmd, .. }
                    if cmd.iter().any(|s| s.contains(PROFILE_D_CREDENTIALS_PATH))
            )
        });
        assert!(
            wrote_profile_d,
            "Expected an exec that writes {}; got: {:?}",
            PROFILE_D_CREDENTIALS_PATH,
            provider.get_calls()
        );

        // Separately verify the script content (which is embedded in the
        // binary, not observable via the mock) actually exports GH_TOKEN.
        assert!(
            PROFILE_D_CREDENTIALS_SCRIPT.contains("GH_TOKEN"),
            "profile.d script content must export GH_TOKEN; got: {}",
            PROFILE_D_CREDENTIALS_SCRIPT
        );
        assert!(
            PROFILE_D_CREDENTIALS_SCRIPT.contains("/run/devc-creds/gh-token"),
            "profile.d script must read from the gh-token cache path"
        );
    }

    /// Lifecycle commands must exec via a login shell (`sh -lc`) so that
    /// /etc/profile, /etc/profile.d/*.sh and user profile scripts are sourced.
    /// That's how our /etc/profile.d/50-devc-credentials.sh gets applied
    /// (exporting GH_TOKEN) and how feature-installed PATH additions (nvm, asdf,
    /// cargo, etc.) become visible inside lifecycle scripts, matching the
    /// user's interactive shell environment.
    #[tokio::test]
    async fn test_lifecycle_string_command_uses_login_shell() {
        use crate::test_support::{MockCall, MockProvider};
        use devc_provider::ProviderType;

        let provider = MockProvider::new(ProviderType::Docker);
        let container_id = ContainerId::new("test-container");

        let cmd = devc_config::Command::String("echo hello".to_string());
        crate::run_lifecycle_command_with_env(&provider, &container_id, &cmd, None, None, None)
            .await
            .expect("lifecycle run");

        let login_shell_used = provider.get_calls().iter().any(|c| {
            matches!(
                c,
                MockCall::Exec { cmd, .. }
                    if cmd.len() >= 3
                    && cmd[0] == "/bin/sh"
                    && cmd[1] == "-lc"
                    && cmd[2] == "echo hello"
            )
        });
        assert!(
            login_shell_used,
            "Expected lifecycle exec to use `/bin/sh -lc`; got: {:?}",
            provider.get_calls()
        );
    }
}
