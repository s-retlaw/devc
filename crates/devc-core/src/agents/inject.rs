use crate::agents::host::{
    host_config_availability, resolve_container_path, validate_host_prerequisites,
};
use crate::agents::{
    cursor_auth::CursorAuthResolution, enabled_agent_configs, AgentKind, AgentSyncResult,
    EffectiveAgentConfig,
};
use devc_config::GlobalConfig;
use devc_provider::{ContainerId, ContainerProvider, ExecConfig};
use std::collections::HashMap;

fn shell_escape_single_quotes(s: &str) -> String {
    s.replace('\'', "'\\''")
}

async fn exec_script(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    script: &str,
    user: Option<&str>,
    env: &HashMap<String, String>,
) -> Result<(i64, String), devc_provider::ProviderError> {
    let result = provider
        .exec(
            container_id,
            &ExecConfig {
                cmd: vec!["/bin/sh".to_string(), "-lc".to_string(), script.to_string()],
                env: env.clone(),
                working_dir: None,
                user: user.map(|u| u.to_string()),
                tty: false,
                stdin: false,
                privileged: false,
            },
        )
        .await?;
    Ok((result.exit_code, result.output))
}

async fn discover_container_home(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    user: Option<&str>,
) -> String {
    match exec_script(
        provider,
        container_id,
        "printf '%s' \"$HOME\"",
        user,
        &HashMap::new(),
    )
    .await
    {
        Ok((0, output)) if !output.trim().is_empty() => output.trim().to_string(),
        _ => {
            if user == Some("root") {
                "/root".to_string()
            } else {
                "/home/vscode".to_string()
            }
        }
    }
}

async fn discover_container_user(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    user: Option<&str>,
) -> String {
    match exec_script(provider, container_id, "id -un", user, &HashMap::new()).await {
        Ok((0, output)) if !output.trim().is_empty() => output.trim().to_string(),
        _ => user.unwrap_or("root").to_string(),
    }
}

async fn copy_sync_entry(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    source_path: &std::path::Path,
    target_path: &str,
) -> Result<(), String> {
    if let Some(parent) = std::path::Path::new(target_path).parent() {
        let quoted_parent = shell_escape_single_quotes(&parent.display().to_string());
        exec_script(
            provider,
            container_id,
            &format!("mkdir -p '{}'", quoted_parent),
            Some("root"),
            &HashMap::new(),
        )
        .await
        .map_err(|e| format!("Failed to create container target directory: {}", e))?;
    }

    provider
        .copy_into(container_id, source_path, target_path)
        .await
        .map_err(|e| format!("Failed to copy host config into container: {}", e))
}

async fn apply_ownership_for_entry(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    target_path: &str,
    container_user: &str,
) -> Result<(), String> {
    if container_user.is_empty() {
        return Ok(());
    }
    let q_target = shell_escape_single_quotes(target_path);
    let q_user = shell_escape_single_quotes(container_user);
    let script = format!(
        "if [ -e '{q_target}' ]; then chown -R '{q_user}:{q_user}' '{q_target}' 2>/dev/null || chown -R '{q_user}' '{q_target}'; fi"
    );
    exec_script(
        provider,
        container_id,
        &script,
        Some("root"),
        &HashMap::new(),
    )
    .await
    .map(|_| ())
    .map_err(|e| format!("Failed to set ownership on synced files: {}", e))
}

fn file_mode_for_name(name: &str) -> Option<&'static str> {
    if name == ".credentials.json" || name == ".claude.json" || name == "auth.json" {
        Some("600")
    } else if name == "settings.json" {
        Some("644")
    } else {
        None
    }
}

async fn apply_permissions_for_entry(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    source_path: &std::path::Path,
    target_path: &str,
) -> Result<(), String> {
    let mut cmds: Vec<String> = Vec::new();
    if source_path.is_file() {
        if let Some(name) = source_path.file_name().and_then(|n| n.to_str()) {
            if let Some(mode) = file_mode_for_name(name) {
                let q = shell_escape_single_quotes(target_path);
                cmds.push(format!("if [ -f '{q}' ]; then chmod {mode} '{q}'; fi"));
            }
        }
    } else if source_path.is_dir() {
        for (name, mode) in [
            (".credentials.json", "600"),
            ("settings.json", "644"),
            (".claude.json", "600"),
        ] {
            let child = format!("{}/{}", target_path.trim_end_matches('/'), name);
            let q = shell_escape_single_quotes(&child);
            cmds.push(format!("if [ -f '{q}' ]; then chmod {mode} '{q}'; fi"));
        }
    }

    if cmds.is_empty() {
        return Ok(());
    }

    exec_script(
        provider,
        container_id,
        &cmds.join(" && "),
        Some("root"),
        &HashMap::new(),
    )
    .await
    .map(|_| ())
    .map_err(|e| format!("Failed to set permissions on synced files: {}", e))
}

fn probe_script(binary: &str) -> String {
    format!("command -v {binary} >/dev/null 2>&1 || [ -x \"$HOME/.local/bin/{binary}\" ]")
}

fn local_prefix_install_command(npm_package: &str) -> String {
    let pkg = shell_escape_single_quotes(npm_package);
    format!("npm install -g --prefix \"$HOME/.local\" '{pkg}'")
}

async fn ensure_local_bin_path(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    user: Option<&str>,
    env: &HashMap<String, String>,
) -> Result<(), String> {
    let script = r#"
mkdir -p "$HOME/.local/bin"
for rc in "$HOME/.profile" "$HOME/.bashrc" "$HOME/.zshrc"; do
  [ -f "$rc" ] || touch "$rc"
  grep -F 'export PATH="$HOME/.local/bin:$PATH"' "$rc" >/dev/null 2>&1 ||
    printf '\nexport PATH="$HOME/.local/bin:$PATH"\n' >> "$rc"
done
"#;

    exec_script(provider, container_id, script, user, env)
        .await
        .map(|_| ())
        .map_err(|e| format!("Failed to ensure ~/.local/bin is on PATH: {}", e))
}

async fn run_install_with_fallbacks(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    cfg: &EffectiveAgentConfig,
    user: Option<&str>,
    env: &HashMap<String, String>,
) -> Result<bool, String> {
    let mut attempts: Vec<String> = vec![cfg.install_command.clone()];
    if let Some(pkg) = &cfg.npm_package {
        let local_prefix_cmd = local_prefix_install_command(pkg);
        if !attempts.iter().any(|cmd| cmd == &local_prefix_cmd) {
            attempts.push(local_prefix_cmd);
        }
    }

    for (idx, cmd) in attempts.iter().enumerate() {
        match exec_script(provider, container_id, cmd, user, env).await {
            Ok((0, _)) => {
                let probe_cmd = probe_script(&cfg.binary_probe);
                match exec_script(provider, container_id, &probe_cmd, user, env).await {
                    Ok((0, _)) => return Ok(true),
                    Ok((code, _)) => {
                        return Err(format!(
                            "Install attempt {} succeeded but probe failed with exit {}",
                            idx + 1,
                            code
                        ));
                    }
                    Err(e) => return Err(format!("Post-install probe failed: {}", e)),
                }
            }
            Ok((code, output)) => {
                let short = output
                    .trim()
                    .lines()
                    .last()
                    .unwrap_or("")
                    .chars()
                    .take(180)
                    .collect::<String>();
                if idx + 1 == attempts.len() {
                    return Err(format!(
                        "Install attempts exhausted (last exit {}): {}. Hint: check npm, network, and writable install prefix.",
                        code, short
                    ));
                }
            }
            Err(e) => {
                if idx + 1 == attempts.len() {
                    return Err(format!(
                        "Install attempts exhausted with runtime error: {}. Hint: check npm, network, and writable install prefix.",
                        e
                    ));
                }
            }
        }
    }

    Ok(false)
}

async fn node_npm_available(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    user: Option<&str>,
    env: &HashMap<String, String>,
) -> Result<bool, String> {
    exec_script(
        provider,
        container_id,
        "command -v node >/dev/null 2>&1 && command -v npm >/dev/null 2>&1",
        user,
        env,
    )
    .await
    .map(|(code, _)| code == 0)
    .map_err(|e| format!("Failed to check Node/npm prerequisites: {}", e))
}

fn all_sync_entries<'a>(
    cfg: &'a EffectiveAgentConfig,
    container_home: &str,
) -> Vec<(&'a std::path::Path, String)> {
    let mut entries = vec![(
        cfg.host_config_path.as_path(),
        resolve_container_path(&cfg.container_config_path, container_home),
    )];
    for (host_path, container_path) in &cfg.extra_sync_paths {
        entries.push((
            host_path.as_path(),
            resolve_container_path(container_path, container_home),
        ));
    }
    entries
}

async fn inject_cursor_auth_file(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    container_home: &str,
    container_user: &str,
    cursor_auth: &CursorAuthResolution,
) -> Result<(), String> {
    let tmp = tempfile::tempdir().map_err(|e| format!("Failed to create temp dir: {}", e))?;
    let auth_file = tmp.path().join("auth.json");
    let payload = serde_json::json!({
        "accessToken": cursor_auth.tokens.auth_token,
        "refreshToken": cursor_auth.tokens.refresh_token,
    });
    let bytes = serde_json::to_vec_pretty(&payload)
        .map_err(|e| format!("Failed to build Cursor auth json: {}", e))?;
    std::fs::write(&auth_file, bytes)
        .map_err(|e| format!("Failed to write temp Cursor auth file: {}", e))?;

    let target = resolve_container_path("~/.config/cursor/auth.json", container_home);
    copy_sync_entry(provider, container_id, &auth_file, &target).await?;
    apply_ownership_for_entry(provider, container_id, &target, container_user).await?;
    apply_permissions_for_entry(provider, container_id, &auth_file, &target).await?;
    Ok(())
}

/// Sync enabled agents into a running container.
///
/// Failures are converted into warnings per-agent; this function is best-effort.
pub async fn setup_agents(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    global_config: &GlobalConfig,
    user: Option<&str>,
) -> Vec<AgentSyncResult> {
    let mut results = Vec::new();
    let enabled = enabled_agent_configs(global_config);

    if enabled.is_empty() {
        return results;
    }

    let container_home = discover_container_home(provider, container_id, user).await;
    let container_user = discover_container_user(provider, container_id, user).await;
    for cfg in enabled {
        let mut result = AgentSyncResult::new(cfg.kind);
        let (available, reason) = host_config_availability(&cfg);
        if !available {
            result.validated = false;
            result.warnings.push(format!(
                "Skipped '{}': host config not available ({})",
                cfg.kind,
                reason.unwrap_or_else(|| "unknown reason".to_string())
            ));
            results.push(result);
            continue;
        }

        let validation = validate_host_prerequisites(&cfg);
        result.validated = validation.valid;
        result.warnings.extend(validation.warnings);

        if !result.validated {
            results.push(result);
            continue;
        }

        for (source_path, target_path) in all_sync_entries(&cfg, &container_home) {
            if let Err(e) = copy_sync_entry(provider, container_id, source_path, &target_path).await
            {
                result.warnings.push(e);
            } else {
                result.copied = true;
                if let Err(e) =
                    apply_ownership_for_entry(provider, container_id, &target_path, &container_user)
                        .await
                {
                    result.warnings.push(e);
                }
                if let Err(e) =
                    apply_permissions_for_entry(provider, container_id, source_path, &target_path)
                        .await
                {
                    result.warnings.push(e);
                }
            }
        }

        if cfg.kind == AgentKind::Cursor {
            if let Some(cursor_auth) = validation.cursor_auth.as_ref() {
                match inject_cursor_auth_file(
                    provider,
                    container_id,
                    &container_home,
                    &container_user,
                    cursor_auth,
                )
                .await
                {
                    Ok(()) => {
                        result.copied = true;
                        tracing::debug!(
                            "Cursor auth materialized from {}",
                            cursor_auth.source.as_str()
                        );
                    }
                    Err(e) => result.warnings.push(format!(
                        "Failed to inject Cursor auth.json from {}: {}",
                        cursor_auth.source.as_str(),
                        e
                    )),
                }
            } else {
                result
                    .warnings
                    .push("Cursor token resolution unavailable; skipped ~/.config/cursor/auth.json materialization".to_string());
            }
        }

        if !result.copied {
            results.push(result);
            continue;
        }

        if let Err(e) =
            ensure_local_bin_path(provider, container_id, user, &validation.forwarded_env).await
        {
            result.warnings.push(e);
        }

        let probe_cmd = probe_script(&cfg.binary_probe);
        let probe_exit = match exec_script(
            provider,
            container_id,
            &probe_cmd,
            user,
            &validation.forwarded_env,
        )
        .await
        {
            Ok((code, _)) => code,
            Err(e) => {
                result.warnings.push(format!(
                    "Failed to probe agent binary '{}': {}",
                    cfg.binary_probe, e
                ));
                results.push(result);
                continue;
            }
        };

        if probe_exit == 0 {
            result.installed = false;
            results.push(result);
            continue;
        }

        let can_install =
            match node_npm_available(provider, container_id, user, &validation.forwarded_env).await
            {
                Ok(v) => v,
                Err(e) => {
                    result.warnings.push(e);
                    false
                }
            };
        if !can_install {
            result.warnings.push(format!(
                "Install skipped for '{}': Node/npm not found in container image",
                cfg.kind
            ));
            results.push(result);
            continue;
        }

        match run_install_with_fallbacks(
            provider,
            container_id,
            &cfg,
            user,
            &validation.forwarded_env,
        )
        .await
        {
            Ok(true) => result.installed = true,
            Ok(false) => result.warnings.push(format!(
                "Install completed but '{}' binary is still unavailable",
                cfg.kind
            )),
            Err(e) => result
                .warnings
                .push(format!("Install failed for '{}': {}", cfg.kind, e)),
        }

        results.push(result);
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{MockCall, MockProvider};
    use devc_provider::ProviderType;
    use std::sync::Mutex;

    static HOME_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[tokio::test]
    async fn test_setup_agents_install_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let host_dir = tmp.path().join("codex");
        std::fs::create_dir_all(&host_dir).unwrap();
        std::fs::write(host_dir.join("auth.json"), "{}").unwrap();

        let mut cfg = GlobalConfig::default();
        cfg.agents.codex.enabled = Some(true);
        cfg.agents.claude.enabled = Some(false);
        cfg.agents.cursor.enabled = Some(false);
        cfg.agents.gemini.enabled = Some(false);
        cfg.agents.codex.host_config_path = Some(host_dir.display().to_string());
        cfg.agents.codex.container_config_path = Some("/tmp/.codex".to_string());
        cfg.agents.codex.install_command = Some("echo installed".to_string());

        let mock = MockProvider::new(ProviderType::Docker);
        mock.exec_responses
            .lock()
            .unwrap()
            .push((0, "/root".to_string())); // HOME probe
        mock.exec_responses
            .lock()
            .unwrap()
            .push((0, "root".to_string())); // user probe
        mock.exec_responses.lock().unwrap().push((0, String::new())); // mkdir
        mock.exec_responses.lock().unwrap().push((0, String::new())); // chown synced files
        mock.exec_responses.lock().unwrap().push((0, String::new())); // chmod synced files
        mock.exec_responses.lock().unwrap().push((0, String::new())); // PATH bootstrap
        mock.exec_responses.lock().unwrap().push((1, String::new())); // command -v fails
        mock.exec_responses.lock().unwrap().push((0, String::new())); // node/npm present
        mock.exec_responses.lock().unwrap().push((0, String::new())); // install succeeds
        mock.exec_responses.lock().unwrap().push((0, String::new())); // post-install probe

        let results = setup_agents(&mock, &ContainerId::new("cid"), &cfg, Some("root")).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].validated);
        assert!(results[0].copied);
        assert!(results[0].installed);

        let calls = mock.get_calls();
        assert!(calls.iter().any(|c| matches!(c, MockCall::CopyInto { .. })));
    }

    #[tokio::test]
    async fn test_setup_agents_missing_host_path_is_warning() {
        let mut cfg = GlobalConfig::default();
        cfg.agents.codex.enabled = Some(true);
        cfg.agents.claude.enabled = Some(false);
        cfg.agents.cursor.enabled = Some(false);
        cfg.agents.gemini.enabled = Some(false);
        cfg.agents.codex.host_config_path = Some("/tmp/devc-no-agent-material".to_string());

        let mock = MockProvider::new(ProviderType::Docker);
        mock.exec_responses
            .lock()
            .unwrap()
            .push((0, "/root".to_string())); // HOME probe
        mock.exec_responses
            .lock()
            .unwrap()
            .push((0, "root".to_string())); // user probe

        let results = setup_agents(&mock, &ContainerId::new("cid"), &cfg, Some("root")).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].warnings.is_empty());
        assert!(!results[0].copied);
    }

    #[tokio::test]
    async fn test_setup_agents_unavailable_skips_copy_install() {
        let mut cfg = GlobalConfig::default();
        cfg.agents.codex.enabled = Some(true);
        cfg.agents.claude.enabled = Some(false);
        cfg.agents.cursor.enabled = Some(false);
        cfg.agents.gemini.enabled = Some(false);
        cfg.agents.codex.host_config_path = Some("/tmp/devc-missing-codex-config".to_string());

        let mock = MockProvider::new(ProviderType::Docker);
        mock.exec_responses
            .lock()
            .unwrap()
            .push((0, "/root".to_string())); // HOME probe
        mock.exec_responses
            .lock()
            .unwrap()
            .push((0, "root".to_string())); // user probe

        let results = setup_agents(&mock, &ContainerId::new("cid"), &cfg, Some("root")).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].validated);
        assert!(!results[0].copied);
        assert!(!results[0].installed);
        assert!(results[0]
            .warnings
            .iter()
            .any(|w| w.contains("Skipped 'codex': host config not available")));

        let calls = mock.get_calls();
        assert!(!calls.iter().any(|c| matches!(c, MockCall::CopyInto { .. })));
    }

    #[tokio::test]
    async fn test_setup_agents_claude_copies_primary_and_extra_paths() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let claude_dir = home.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join(".credentials.json"), "{}").unwrap();
        std::fs::write(claude_dir.join("settings.json"), "{}").unwrap();
        std::fs::write(home.join(".claude.json"), "{}").unwrap();

        let old_home = std::env::var("HOME").ok();
        // SAFETY: test-local environment setup for path expansion; restored below.
        unsafe { std::env::set_var("HOME", home.display().to_string()) };

        let mut cfg = GlobalConfig::default();
        cfg.agents.claude.enabled = Some(true);
        cfg.agents.codex.enabled = Some(false);
        cfg.agents.cursor.enabled = Some(false);
        cfg.agents.gemini.enabled = Some(false);

        let mock = MockProvider::new(ProviderType::Docker);
        mock.exec_responses
            .lock()
            .unwrap()
            .push((0, "/root".to_string())); // HOME probe
        mock.exec_responses
            .lock()
            .unwrap()
            .push((0, "root".to_string())); // user probe
        mock.exec_responses.lock().unwrap().push((0, String::new())); // mkdir #1
        mock.exec_responses.lock().unwrap().push((0, String::new())); // mkdir #2
        mock.exec_responses.lock().unwrap().push((0, String::new())); // chown for ~/.claude
        mock.exec_responses.lock().unwrap().push((0, String::new())); // chown for ~/.claude.json
        mock.exec_responses.lock().unwrap().push((0, String::new())); // chmod for ~/.claude
        mock.exec_responses.lock().unwrap().push((0, String::new())); // chmod for ~/.claude.json
        mock.exec_responses.lock().unwrap().push((0, String::new())); // PATH bootstrap
        mock.exec_responses.lock().unwrap().push((0, String::new())); // command -v ok

        let results = setup_agents(&mock, &ContainerId::new("cid"), &cfg, Some("root")).await;

        if let Some(old) = old_home {
            // SAFETY: restore HOME after test.
            unsafe { std::env::set_var("HOME", old) };
        } else {
            // SAFETY: restore HOME to unset state after test.
            unsafe { std::env::remove_var("HOME") };
        }

        assert_eq!(results.len(), 1);
        assert!(results[0].copied);

        let calls = mock.get_calls();
        let copy_dests: Vec<String> = calls
            .iter()
            .filter_map(|c| match c {
                MockCall::CopyInto { dest, .. } => Some(dest.clone()),
                _ => None,
            })
            .collect();
        assert!(copy_dests.iter().any(|d| d.ends_with("/.claude")));
        assert!(copy_dests.iter().any(|d| d.ends_with("/.claude.json")));
    }

    #[tokio::test]
    async fn test_setup_agents_codex_install_fallback_succeeds() {
        let tmp = tempfile::tempdir().unwrap();
        let host_dir = tmp.path().join("codex");
        std::fs::create_dir_all(&host_dir).unwrap();
        std::fs::write(host_dir.join("auth.json"), "{}").unwrap();

        let mut cfg = GlobalConfig::default();
        cfg.agents.codex.enabled = Some(true);
        cfg.agents.claude.enabled = Some(false);
        cfg.agents.cursor.enabled = Some(false);
        cfg.agents.gemini.enabled = Some(false);
        cfg.agents.codex.host_config_path = Some(host_dir.display().to_string());
        cfg.agents.codex.install_command = Some("echo primary-install && exit 7".to_string());

        let mock = MockProvider::new(ProviderType::Docker);
        mock.exec_responses
            .lock()
            .unwrap()
            .push((0, "/root".to_string())); // HOME probe
        mock.exec_responses
            .lock()
            .unwrap()
            .push((0, "root".to_string())); // user probe
        mock.exec_responses.lock().unwrap().push((0, String::new())); // mkdir
        mock.exec_responses.lock().unwrap().push((0, String::new())); // chown
        mock.exec_responses.lock().unwrap().push((0, String::new())); // chmod
        mock.exec_responses.lock().unwrap().push((0, String::new())); // PATH bootstrap
        mock.exec_responses.lock().unwrap().push((1, String::new())); // initial probe missing
        mock.exec_responses.lock().unwrap().push((0, String::new())); // node/npm present
        mock.exec_responses
            .lock()
            .unwrap()
            .push((7, "primary failed".to_string())); // install attempt 1 fails
        mock.exec_responses.lock().unwrap().push((0, String::new())); // fallback install succeeds
        mock.exec_responses.lock().unwrap().push((0, String::new())); // post-install probe succeeds

        let results = setup_agents(&mock, &ContainerId::new("cid"), &cfg, Some("root")).await;
        assert_eq!(results.len(), 1);
        assert!(
            results[0].installed,
            "fallback install should mark installed"
        );

        let exec_cmds: Vec<Vec<String>> = mock
            .get_calls()
            .iter()
            .filter_map(|c| match c {
                MockCall::Exec { cmd, .. } => Some(cmd.clone()),
                _ => None,
            })
            .collect();
        assert!(
            exec_cmds.iter().any(|cmd| cmd
                .iter()
                .any(|c| c.contains("--prefix \"$HOME/.local\"") && c.contains("@openai/codex"))),
            "expected codex fallback install command, got: {:?}",
            exec_cmds
        );
    }

    #[tokio::test]
    async fn test_setup_agents_skips_install_when_node_npm_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let host_dir = tmp.path().join("codex");
        std::fs::create_dir_all(&host_dir).unwrap();
        std::fs::write(host_dir.join("auth.json"), "{}").unwrap();

        let mut cfg = GlobalConfig::default();
        cfg.agents.codex.enabled = Some(true);
        cfg.agents.claude.enabled = Some(false);
        cfg.agents.cursor.enabled = Some(false);
        cfg.agents.gemini.enabled = Some(false);
        cfg.agents.codex.host_config_path = Some(host_dir.display().to_string());
        cfg.agents.codex.install_command = Some("echo should-not-run".to_string());

        let mock = MockProvider::new(ProviderType::Docker);
        mock.exec_responses
            .lock()
            .unwrap()
            .push((0, "/root".to_string())); // HOME probe
        mock.exec_responses
            .lock()
            .unwrap()
            .push((0, "root".to_string())); // user probe
        mock.exec_responses.lock().unwrap().push((0, String::new())); // mkdir
        mock.exec_responses.lock().unwrap().push((0, String::new())); // chown
        mock.exec_responses.lock().unwrap().push((0, String::new())); // chmod
        mock.exec_responses.lock().unwrap().push((0, String::new())); // PATH bootstrap
        mock.exec_responses.lock().unwrap().push((1, String::new())); // initial probe missing
        mock.exec_responses.lock().unwrap().push((1, String::new())); // node/npm missing

        let results = setup_agents(&mock, &ContainerId::new("cid"), &cfg, Some("root")).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].installed);
        assert!(results[0]
            .warnings
            .iter()
            .any(|w| w.contains("Node/npm not found in container image")));

        let exec_cmds: Vec<Vec<String>> = mock
            .get_calls()
            .iter()
            .filter_map(|c| match c {
                MockCall::Exec { cmd, .. } => Some(cmd.clone()),
                _ => None,
            })
            .collect();
        assert!(
            !exec_cmds
                .iter()
                .any(|cmd| cmd.iter().any(|s| s.contains("should-not-run"))),
            "install override should not run when Node/npm are missing"
        );
    }

    #[tokio::test]
    async fn test_setup_agents_cursor_materializes_config_auth_json() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cursor_host = home.join(".cursor");
        std::fs::create_dir_all(&cursor_host).unwrap();
        std::fs::write(cursor_host.join("settings.json"), "{}").unwrap();
        let cursor_cfg = home.join(".config/cursor");
        std::fs::create_dir_all(&cursor_cfg).unwrap();
        std::fs::write(
            cursor_cfg.join("auth.json"),
            r#"{"accessToken":"a-token","refreshToken":"r-token"}"#,
        )
        .unwrap();

        let old_home = std::env::var("HOME").ok();
        // SAFETY: test-local HOME override for token resolution, restored below.
        unsafe { std::env::set_var("HOME", home.display().to_string()) };

        let mut cfg = GlobalConfig::default();
        cfg.agents.cursor.enabled = Some(true);
        cfg.agents.codex.enabled = Some(false);
        cfg.agents.claude.enabled = Some(false);
        cfg.agents.gemini.enabled = Some(false);
        cfg.agents.cursor.host_config_path = Some(cursor_host.display().to_string());

        let mock = MockProvider::new(ProviderType::Docker);
        mock.exec_responses
            .lock()
            .unwrap()
            .push((0, "/root".to_string())); // HOME probe
        mock.exec_responses
            .lock()
            .unwrap()
            .push((0, "root".to_string())); // user probe
        mock.exec_responses.lock().unwrap().push((0, String::new())); // mkdir for ~/.cursor
        mock.exec_responses.lock().unwrap().push((0, String::new())); // chown for ~/.cursor
        mock.exec_responses.lock().unwrap().push((0, String::new())); // chmod for ~/.cursor
        mock.exec_responses.lock().unwrap().push((0, String::new())); // mkdir for ~/.config/cursor
        mock.exec_responses.lock().unwrap().push((0, String::new())); // chown for auth.json
        mock.exec_responses.lock().unwrap().push((0, String::new())); // chmod for auth.json
        mock.exec_responses.lock().unwrap().push((0, String::new())); // PATH bootstrap
        mock.exec_responses.lock().unwrap().push((0, String::new())); // binary probe succeeds

        let results = setup_agents(&mock, &ContainerId::new("cid"), &cfg, Some("root")).await;
        if let Some(old) = old_home {
            // SAFETY: restore HOME after test.
            unsafe { std::env::set_var("HOME", old) };
        } else {
            // SAFETY: restore HOME to unset state after test.
            unsafe { std::env::remove_var("HOME") };
        }

        assert_eq!(results.len(), 1);
        assert!(results[0].copied);
        assert!(
            !results[0]
                .warnings
                .iter()
                .any(|w| w.contains("Cursor token resolution failed")),
            "unexpected token resolution warning: {:?}",
            results[0].warnings
        );

        let calls = mock.get_calls();
        let copy_dests: Vec<String> = calls
            .iter()
            .filter_map(|c| match c {
                MockCall::CopyInto { dest, .. } => Some(dest.clone()),
                _ => None,
            })
            .collect();
        assert!(
            copy_dests
                .iter()
                .any(|d| d.ends_with("/.config/cursor/auth.json")),
            "expected cursor auth.json to be copied; got {:?}",
            copy_dests
        );
    }
}
