//! Agent injection orchestration and host/container sync helpers.

mod cursor_auth;
mod host;
mod inject;
mod presets;

use devc_config::{AgentConfig, GlobalConfig};
use std::path::PathBuf;

pub use host::{
    doctor_enabled_agents, host_agent_availability, host_config_availability,
    validate_host_prerequisites, HostValidation,
};
pub use inject::{inspect_agents, setup_agents, setup_agents_with_selection};
pub use presets::{preset_for, AgentKind, AgentPreset};

/// Effective config for an enabled agent after applying preset defaults + user overrides.
#[derive(Debug, Clone)]
pub struct EffectiveAgentConfig {
    pub kind: AgentKind,
    pub host_config_path: PathBuf,
    pub container_config_path: String,
    pub extra_sync_paths: Vec<(PathBuf, String)>,
    pub npm_package: Option<String>,
    pub env_forward: Vec<String>,
    pub required_env_keys: Vec<String>,
    pub binary_probe: String,
    pub install_command: String,
}

/// Per-agent sync result used by lifecycle logs and CLI diagnostics.
#[derive(Debug, Clone)]
pub struct AgentSyncResult {
    pub agent: AgentKind,
    pub validated: bool,
    pub copied: bool,
    pub installed: bool,
    pub warnings: Vec<String>,
}

/// Host-side availability for an agent (based on host config material presence/readability).
#[derive(Debug, Clone)]
pub struct HostAgentAvailability {
    pub agent: AgentKind,
    pub available: bool,
    pub reason: Option<String>,
}

/// Container-side presence details per agent for richer diagnostics UX.
#[derive(Debug, Clone)]
pub struct AgentContainerPresence {
    pub agent: AgentKind,
    pub enabled_effective: bool,
    pub enabled_explicit: Option<bool>,
    pub host_available: bool,
    pub host_reason: Option<String>,
    pub container_config_present: bool,
    pub container_binary_present: bool,
    pub warnings: Vec<String>,
}

/// Agent selection mode for sync operations.
#[derive(Debug, Clone)]
pub enum AgentSyncSelection {
    EnabledOnly,
    Only(Vec<AgentKind>),
    ForceOnly(Vec<AgentKind>),
}

impl AgentSyncResult {
    pub fn new(agent: AgentKind) -> Self {
        Self {
            agent,
            validated: false,
            copied: false,
            installed: false,
            warnings: Vec::new(),
        }
    }
}

/// Return effective configs for all supported agents (enabled and disabled).
pub fn all_agent_configs(global_config: &GlobalConfig) -> Vec<EffectiveAgentConfig> {
    AgentKind::ALL
        .into_iter()
        .map(|kind| {
            let cfg = agent_config_for_kind(&global_config.agents, kind);
            resolve_effective_config(kind, cfg)
        })
        .collect()
}

/// Return effective configs for all enabled agents.
pub fn enabled_agent_configs(global_config: &GlobalConfig) -> Vec<EffectiveAgentConfig> {
    all_agent_configs(global_config)
        .into_iter()
        .filter(|cfg| is_agent_enabled(global_config, cfg.kind, Some(cfg)))
        .collect()
}

/// Return effective configs selected for sync based on the requested scope.
pub fn selected_agent_configs(
    global_config: &GlobalConfig,
    selection: &AgentSyncSelection,
) -> Vec<EffectiveAgentConfig> {
    let all = all_agent_configs(global_config);
    match selection {
        AgentSyncSelection::EnabledOnly => all
            .into_iter()
            .filter(|cfg| is_agent_enabled(global_config, cfg.kind, Some(cfg)))
            .collect(),
        AgentSyncSelection::Only(kinds) => all
            .into_iter()
            .filter(|cfg| kinds.contains(&cfg.kind))
            .filter(|cfg| is_agent_enabled(global_config, cfg.kind, Some(cfg)))
            .collect(),
        AgentSyncSelection::ForceOnly(kinds) => all
            .into_iter()
            .filter(|cfg| kinds.contains(&cfg.kind))
            .collect(),
    }
}

/// Resolve whether an agent is enabled, honoring explicit override first and host-availability defaults.
pub fn is_agent_enabled(
    global_config: &GlobalConfig,
    kind: AgentKind,
    resolved: Option<&EffectiveAgentConfig>,
) -> bool {
    let cfg = agent_config_for_kind(&global_config.agents, kind);
    match cfg.enabled {
        Some(v) => v,
        None => {
            let effective = resolved
                .cloned()
                .unwrap_or_else(|| resolve_effective_config(kind, cfg));
            host::host_config_availability(&effective).0
        }
    }
}

fn agent_config_for_kind(agents: &devc_config::AgentsConfig, kind: AgentKind) -> &AgentConfig {
    match kind {
        AgentKind::Codex => &agents.codex,
        AgentKind::Claude => &agents.claude,
        AgentKind::Cursor => &agents.cursor,
        AgentKind::Gemini => &agents.gemini,
    }
}

fn resolve_effective_config(kind: AgentKind, cfg: &AgentConfig) -> EffectiveAgentConfig {
    let preset = preset_for(kind);
    let host_path = cfg
        .host_config_path
        .as_deref()
        .unwrap_or(preset.default_host_config_path);
    let container_path = cfg
        .container_config_path
        .clone()
        .unwrap_or_else(|| preset.default_container_config_path.to_string());
    let install_command = cfg
        .install_command
        .clone()
        .unwrap_or_else(|| preset.default_install_command.to_string());
    let extra_sync_paths = preset
        .default_extra_sync_paths
        .iter()
        .map(|(host, container)| (host::expand_home_path(host), (*container).to_string()))
        .collect();
    EffectiveAgentConfig {
        kind,
        host_config_path: host::expand_home_path(host_path),
        container_config_path: container_path,
        extra_sync_paths,
        npm_package: preset.npm_package.map(|pkg| pkg.to_string()),
        env_forward: cfg.env_forward.clone(),
        required_env_keys: preset
            .required_env_keys
            .iter()
            .map(|k| (*k).to_string())
            .collect(),
        binary_probe: preset.binary_probe.to_string(),
        install_command,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enabled_agent_resolution_and_override_precedence() {
        let mut config = GlobalConfig::default();
        config.agents.codex.enabled = Some(true);
        config.agents.claude.enabled = Some(false);
        config.agents.cursor.enabled = Some(false);
        config.agents.gemini.enabled = Some(false);
        config.agents.codex.host_config_path = Some("/tmp/custom-codex".to_string());
        config.agents.codex.container_config_path = Some("/work/.codex".to_string());
        config.agents.codex.install_command = Some("echo custom-install".to_string());

        let effective = enabled_agent_configs(&config);
        assert_eq!(effective.len(), 1);
        let codex = &effective[0];
        assert_eq!(codex.kind, AgentKind::Codex);
        assert_eq!(codex.host_config_path, PathBuf::from("/tmp/custom-codex"));
        assert_eq!(codex.container_config_path, "/work/.codex");
        assert_eq!(codex.install_command, "echo custom-install");
    }

    #[test]
    fn test_disabled_agents_are_filtered() {
        let mut config = GlobalConfig::default();
        config.agents.codex.enabled = Some(false);
        config.agents.claude.enabled = Some(false);
        config.agents.cursor.enabled = Some(false);
        config.agents.gemini.enabled = Some(false);
        assert!(enabled_agent_configs(&config).is_empty());
    }

    #[test]
    fn test_claude_includes_default_extra_sync_path() {
        let mut config = GlobalConfig::default();
        config.agents.claude.enabled = Some(true);
        config.agents.codex.enabled = Some(false);
        config.agents.cursor.enabled = Some(false);
        config.agents.gemini.enabled = Some(false);

        let effective = enabled_agent_configs(&config);
        assert_eq!(effective.len(), 1);
        let claude = &effective[0];
        assert_eq!(claude.kind, AgentKind::Claude);
        assert_eq!(claude.extra_sync_paths.len(), 1);
        assert_eq!(claude.extra_sync_paths[0].1, "~/.claude.json");
    }

    #[test]
    fn test_auto_enabled_when_host_config_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let host_dir = tmp.path().join("codex");
        std::fs::create_dir_all(&host_dir).unwrap();

        let mut config = GlobalConfig::default();
        config.agents.codex.host_config_path = Some(host_dir.display().to_string());
        config.agents.codex.enabled = None;
        config.agents.claude.enabled = Some(false);
        config.agents.cursor.enabled = Some(false);
        config.agents.gemini.enabled = Some(false);

        let effective = enabled_agent_configs(&config);
        assert_eq!(effective.len(), 1);
        assert_eq!(effective[0].kind, AgentKind::Codex);
    }

    #[test]
    fn test_selected_agent_configs_only_respects_enabled_state() {
        let mut config = GlobalConfig::default();
        config.agents.codex.enabled = Some(false);
        config.agents.cursor.enabled = Some(true);
        config.agents.claude.enabled = Some(false);
        config.agents.gemini.enabled = Some(false);

        let selected = selected_agent_configs(
            &config,
            &AgentSyncSelection::Only(vec![AgentKind::Codex, AgentKind::Cursor]),
        );
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].kind, AgentKind::Cursor);
    }

    #[test]
    fn test_selected_agent_configs_force_only_ignores_enabled_flag() {
        let mut config = GlobalConfig::default();
        config.agents.codex.enabled = Some(false);
        config.agents.claude.enabled = Some(false);
        config.agents.cursor.enabled = Some(false);
        config.agents.gemini.enabled = Some(false);

        let selected = selected_agent_configs(
            &config,
            &AgentSyncSelection::ForceOnly(vec![AgentKind::Codex]),
        );
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].kind, AgentKind::Codex);
    }
}
