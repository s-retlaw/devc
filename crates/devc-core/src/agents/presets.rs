use std::fmt;

/// Supported agent kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentKind {
    Codex,
    Claude,
    Cursor,
    Gemini,
}

impl AgentKind {
    pub const ALL: [AgentKind; 4] = [
        AgentKind::Codex,
        AgentKind::Claude,
        AgentKind::Cursor,
        AgentKind::Gemini,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            AgentKind::Codex => "codex",
            AgentKind::Claude => "claude",
            AgentKind::Cursor => "cursor",
            AgentKind::Gemini => "gemini",
        }
    }
}

impl fmt::Display for AgentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Built-in defaults for each supported agent.
#[derive(Debug, Clone)]
pub struct AgentPreset {
    pub kind: AgentKind,
    pub default_host_config_path: &'static str,
    pub default_container_config_path: &'static str,
    pub default_extra_sync_paths: &'static [(&'static str, &'static str)],
    pub npm_package: Option<&'static str>,
    pub required_env_keys: &'static [&'static str],
    pub binary_probe: &'static str,
    pub default_install_command: &'static str,
}

pub fn preset_for(kind: AgentKind) -> AgentPreset {
    match kind {
        AgentKind::Codex => AgentPreset {
            kind,
            default_host_config_path: "~/.codex",
            default_container_config_path: "~/.codex",
            default_extra_sync_paths: &[],
            npm_package: Some("@openai/codex"),
            required_env_keys: &[],
            binary_probe: "codex",
            default_install_command: "npm install -g @openai/codex",
        },
        AgentKind::Claude => AgentPreset {
            kind,
            default_host_config_path: "~/.claude",
            default_container_config_path: "~/.claude",
            default_extra_sync_paths: &[("~/.claude.json", "~/.claude.json")],
            npm_package: Some("@anthropic-ai/claude-code"),
            required_env_keys: &[],
            binary_probe: "claude",
            default_install_command: "npm install -g @anthropic-ai/claude-code",
        },
        AgentKind::Cursor => AgentPreset {
            kind,
            default_host_config_path: "~/.cursor",
            default_container_config_path: "~/.cursor",
            default_extra_sync_paths: &[],
            npm_package: Some("@cursor/agent"),
            required_env_keys: &[],
            binary_probe: "cursor-agent",
            default_install_command: "npm install -g @cursor/agent",
        },
        AgentKind::Gemini => AgentPreset {
            kind,
            default_host_config_path: "~/.gemini",
            default_container_config_path: "~/.gemini",
            default_extra_sync_paths: &[],
            npm_package: Some("@google/gemini-cli"),
            required_env_keys: &[],
            binary_probe: "gemini",
            default_install_command: "npm install -g @google/gemini-cli",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_presets_have_defaults() {
        for kind in AgentKind::ALL {
            let preset = preset_for(kind);
            assert_eq!(preset.kind, kind);
            assert!(!preset.default_host_config_path.is_empty());
            assert!(!preset.default_container_config_path.is_empty());
            assert!(
                kind != AgentKind::Claude || !preset.default_extra_sync_paths.is_empty(),
                "Claude must include extra sync path(s)"
            );
            assert!(!preset.binary_probe.is_empty());
            assert!(!preset.default_install_command.is_empty());
        }
    }
}
