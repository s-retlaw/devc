//! Browser URL forwarding from container to host
//!
//! Injects wrapper scripts for `xdg-open`, `open`, and `sensible-browser`
//! inside containers. When a tool calls these wrappers with a URL argument,
//! they write the URL to a queue file in the shared workspace mount.
//! The host-side relay loop checks this file and opens the URL in the
//! host browser.
//!
//! As a fallback (e.g. when workspace path is unknown), the wrapper emits
//! a custom OSC escape sequence that the PTY relay can intercept.

use crate::{CoreError, Result};
use devc_provider::{ContainerId, ContainerProvider, ExecConfig};
use std::collections::HashMap;

/// The custom OSC prefix used as fallback URL signaling through the PTY.
/// Format: \x1b]devc;open-url;<URL>\x07
pub const OSC_PREFIX: &[u8] = b"\x1b]devc;open-url;";
pub const OSC_TERMINATOR: u8 = 0x07;

/// Name of the browser queue file placed in the workspace root
pub const BROWSER_QUEUE_FILENAME: &str = ".devc-browser-queue";

/// Marker comment in the wrapper script for idempotency checks.
/// Versioned so updated wrappers replace older ones.
const MARKER: &str = "# devc-browser-forwarder-v3";

/// The wrapper script injected into the container.
/// Primary mechanism: writes URL to a queue file in the workspace mount
/// (set via DEVC_BROWSER_QUEUE env var). The host reads this file.
/// Fallback: emits OSC escape sequence for direct PTY interception.
const BROWSER_WRAPPER_SCRIPT: &str = r#"#!/bin/sh
# devc-browser-forwarder-v3
URL="$1"
case "$URL" in
  http://*|https://*|ftp://*)
    if [ -n "$DEVC_BROWSER_QUEUE" ]; then
      echo "$URL" >> "$DEVC_BROWSER_QUEUE"
    else
      # Fallback: OSC escape sequence (works when no tmux in path)
      printf '\033]devc;open-url;%s\007' "$URL"
    fi
    exit 0
    ;;
esac
# Non-URL argument: try the original binary if it was backed up
if [ -x /usr/local/bin/.xdg-open.orig ]; then
  exec /usr/local/bin/.xdg-open.orig "$@"
fi
echo "devc: cannot open '$1' (no browser available in container)" >&2
exit 1
"#;

/// Inject browser forwarder wrapper scripts into the container.
///
/// Places wrapper scripts at `/usr/local/bin/{xdg-open,open,sensible-browser}`
/// that write URLs to a queue file in the workspace mount.
/// Idempotent — skips injection if the marker is already present.
pub async fn inject_browser_forwarder(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
) -> Result<()> {
    // Idempotency: check if already injected
    if is_already_injected(provider, container_id).await {
        return Ok(());
    }

    // Back up existing xdg-open if it exists and isn't already our wrapper
    let backup_script =
        "if [ -x /usr/local/bin/xdg-open ] && ! grep -q 'devc-browser-forwarder' /usr/local/bin/xdg-open 2>/dev/null; then \
         cp /usr/local/bin/xdg-open /usr/local/bin/.xdg-open.orig; \
         fi";
    let _ = exec_script(provider, container_id, backup_script, Some("root")).await;

    // Ensure /usr/local/bin exists
    let _ = exec_script(
        provider,
        container_id,
        "mkdir -p /usr/local/bin",
        Some("root"),
    )
    .await;

    // Write wrapper scripts
    for target in &[
        "/usr/local/bin/xdg-open",
        "/usr/local/bin/open",
        "/usr/local/bin/sensible-browser",
    ] {
        write_script_to_container(provider, container_id, target, BROWSER_WRAPPER_SCRIPT).await?;
    }

    tracing::debug!("Browser forwarder injected into container");
    Ok(())
}

/// Check if the browser forwarder is already injected
async fn is_already_injected(provider: &dyn ContainerProvider, container_id: &ContainerId) -> bool {
    let config = ExecConfig {
        cmd: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("grep -q '{}' /usr/local/bin/xdg-open 2>/dev/null", MARKER),
        ],
        env: HashMap::new(),
        working_dir: None,
        user: Some("root".to_string()),
        tty: false,
        stdin: false,
        privileged: false,
    };

    provider
        .exec(container_id, &config)
        .await
        .map(|r| r.exit_code == 0)
        .unwrap_or(false)
}

/// Write a script to the container using base64 encoding
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
