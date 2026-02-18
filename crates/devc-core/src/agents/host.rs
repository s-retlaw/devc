use crate::agents::{
    all_agent_configs,
    cursor_auth::{resolve_cursor_tokens, CursorAuthResolution},
    enabled_agent_configs, AgentKind, AgentSyncResult, EffectiveAgentConfig, HostAgentAvailability,
};
use devc_config::GlobalConfig;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Host-side validation details for a single agent.
#[derive(Debug, Clone)]
pub struct HostValidation {
    pub valid: bool,
    pub warnings: Vec<String>,
    pub forwarded_env: HashMap<String, String>,
    pub cursor_auth: Option<CursorAuthResolution>,
}

/// Expand `~/...` against current HOME for host paths.
pub(crate) fn expand_home_path(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// Resolve `~/...` against a container home directory.
pub(crate) fn resolve_container_path(path: &str, container_home: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        return format!("{}/{}", container_home.trim_end_matches('/'), rest);
    }
    path.to_string()
}

fn is_readable(path: &Path) -> bool {
    if path.is_dir() {
        std::fs::read_dir(path).is_ok()
    } else {
        std::fs::File::open(path).is_ok()
    }
}

/// Determine whether the primary host config path for an agent is available.
pub fn host_config_availability(cfg: &EffectiveAgentConfig) -> (bool, Option<String>) {
    if !cfg.host_config_path.exists() {
        if cfg.kind == AgentKind::Cursor && resolve_cursor_tokens().is_ok() {
            return (true, None);
        }
        return (
            false,
            Some(format!(
                "host config missing: {}",
                cfg.host_config_path.display()
            )),
        );
    }
    if !is_readable(&cfg.host_config_path) {
        if cfg.kind == AgentKind::Cursor && resolve_cursor_tokens().is_ok() {
            return (true, None);
        }
        return (
            false,
            Some(format!(
                "host config not readable: {}",
                cfg.host_config_path.display()
            )),
        );
    }
    (true, None)
}

/// Validate host prerequisites and collect env forwarding material.
pub fn validate_host_prerequisites(cfg: &EffectiveAgentConfig) -> HostValidation {
    let mut warnings = Vec::new();
    let mut forwarded_env = HashMap::new();
    let mut has_blocking_issue = false;
    let mut cursor_auth = None;

    if !cfg.host_config_path.exists() {
        if cfg.kind == AgentKind::Cursor {
            warnings.push(format!(
                "Host config path is missing (continuing with Cursor token resolution): {}",
                cfg.host_config_path.display()
            ));
        } else {
            warnings.push(format!(
                "Host config path is missing: {}",
                cfg.host_config_path.display()
            ));
            has_blocking_issue = true;
        }
    } else if !is_readable(&cfg.host_config_path) {
        if cfg.kind == AgentKind::Cursor {
            warnings.push(format!(
                "Host config path is not readable (continuing with Cursor token resolution): {}",
                cfg.host_config_path.display()
            ));
        } else {
            warnings.push(format!(
                "Host config path is not readable: {}",
                cfg.host_config_path.display()
            ));
            has_blocking_issue = true;
        }
    }

    for (extra_host_path, _) in &cfg.extra_sync_paths {
        if !extra_host_path.exists() {
            warnings.push(format!(
                "Extra host sync path is missing: {}",
                extra_host_path.display()
            ));
        } else if !is_readable(extra_host_path) {
            warnings.push(format!(
                "Extra host sync path is not readable: {}",
                extra_host_path.display()
            ));
        }
    }

    for key in &cfg.required_env_keys {
        match std::env::var(key) {
            Ok(v) => {
                forwarded_env.insert(key.clone(), v);
            }
            Err(_) => {
                warnings.push(format!("Required host env var is missing: {}", key));
                has_blocking_issue = true;
            }
        }
    }

    for key in &cfg.env_forward {
        if forwarded_env.contains_key(key) {
            continue;
        }
        match std::env::var(key) {
            Ok(v) => {
                forwarded_env.insert(key.clone(), v);
            }
            Err(_) => warnings.push(format!("Allowlisted env var not found: {}", key)),
        }
    }

    if cfg.kind == AgentKind::Cursor {
        match resolve_cursor_tokens() {
            Ok(resolution) => {
                cursor_auth = Some(resolution);
            }
            Err(e) => warnings.push(format!("Cursor token resolution failed: {}", e)),
        }
    }

    HostValidation {
        valid: !has_blocking_issue,
        warnings,
        forwarded_env,
        cursor_auth,
    }
}

/// Host availability inventory for all known agents.
pub fn host_agent_availability(global_config: &GlobalConfig) -> Vec<HostAgentAvailability> {
    all_agent_configs(global_config)
        .into_iter()
        .map(|cfg| {
            let (available, reason) = host_config_availability(&cfg);
            HostAgentAvailability {
                agent: cfg.kind,
                available,
                reason,
            }
        })
        .collect()
}

/// Host-only diagnostic for enabled agents.
pub fn doctor_enabled_agents(global_config: &GlobalConfig) -> Vec<AgentSyncResult> {
    enabled_agent_configs(global_config)
        .into_iter()
        .map(|cfg| {
            let mut result = AgentSyncResult::new(cfg.kind);
            let validation = validate_host_prerequisites(&cfg);
            result.validated = validation.valid;
            result.warnings = validation.warnings;
            result
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;
    use std::sync::Mutex;

    static HOME_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_resolve_container_path_with_home_marker() {
        assert_eq!(
            resolve_container_path("~/.codex", "/home/vscode"),
            "/home/vscode/.codex"
        );
        assert_eq!(
            resolve_container_path("/etc/codex", "/home/vscode"),
            "/etc/codex"
        );
    }

    #[test]
    fn test_validate_host_prerequisites_missing_path() {
        let cfg = EffectiveAgentConfig {
            kind: AgentKind::Codex,
            host_config_path: PathBuf::from("/tmp/devc-definitely-missing-agent-dir"),
            container_config_path: "/home/vscode/.codex".to_string(),
            extra_sync_paths: Vec::new(),
            npm_package: Some("@openai/codex".to_string()),
            env_forward: vec!["DEVC_TEST_ENV_MISSING".to_string()],
            required_env_keys: vec!["DEVC_TEST_REQ_ENV_MISSING".to_string()],
            binary_probe: "codex".to_string(),
            install_command: "echo install".to_string(),
        };
        let validation = validate_host_prerequisites(&cfg);
        assert!(!validation.valid);
        assert!(validation
            .warnings
            .iter()
            .any(|w| w.contains("Host config path is missing")));
        assert!(validation
            .warnings
            .iter()
            .any(|w| w.contains("Required host env var is missing")));
    }

    #[test]
    fn test_host_agent_availability_config_missing() {
        let mut config = GlobalConfig::default();
        config.agents.codex.enabled = Some(true);
        config.agents.codex.host_config_path = Some("/tmp/devc-missing-host-config".to_string());

        let all = host_agent_availability(&config);
        let codex = all
            .iter()
            .find(|a| a.agent == AgentKind::Codex)
            .expect("codex availability should be present");
        assert!(!codex.available);
        assert!(codex
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("host config missing"));
    }

    #[test]
    fn test_cursor_host_availability_uses_resolved_tokens() {
        let _guard = HOME_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cursor_cfg_dir = home.join(".config/cursor");
        std::fs::create_dir_all(&cursor_cfg_dir).unwrap();
        std::fs::write(
            cursor_cfg_dir.join("auth.json"),
            r#"{"accessToken":"a-token","refreshToken":"r-token"}"#,
        )
        .unwrap();
        let old_home = std::env::var("HOME").ok();
        // SAFETY: test-local HOME override; restored before test exits.
        unsafe { std::env::set_var("HOME", home.display().to_string()) };

        let cfg = EffectiveAgentConfig {
            kind: AgentKind::Cursor,
            host_config_path: PathBuf::from("/tmp/devc-cursor-missing-dir"),
            container_config_path: "~/.cursor".to_string(),
            extra_sync_paths: Vec::new(),
            npm_package: Some("@cursor/agent".to_string()),
            env_forward: Vec::new(),
            required_env_keys: Vec::new(),
            binary_probe: "cursor-agent".to_string(),
            install_command: "echo install".to_string(),
        };

        let (available, reason) = host_config_availability(&cfg);
        if let Some(old) = old_home {
            // SAFETY: restore HOME after test.
            unsafe { std::env::set_var("HOME", old) };
        } else {
            // SAFETY: restore HOME to unset state after test.
            unsafe { std::env::remove_var("HOME") };
        }

        assert!(available);
        assert!(reason.is_none());
    }
}
