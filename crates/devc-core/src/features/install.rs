//! Exec-based feature installer for compose containers
//!
//! For compose-based devcontainers, features can't be baked into images at build time
//! (the build is managed by `docker compose up --build`). Instead, we copy each feature
//! into the running container and run its install.sh via exec.

use crate::Result;
use devc_provider::{ContainerId, ContainerProvider, ExecConfig};
use std::collections::HashMap;
use tokio::sync::mpsc;

use super::resolve::ResolvedFeature;

/// Install features into a running container via exec.
///
/// For each feature:
/// 1. Copy the feature directory into the container at `/tmp/dev-container-feature/`
/// 2. Run `install.sh` with the feature's options as environment variables
/// 3. Clean up the temporary directory
///
/// After all features are installed, write any `containerEnv` entries to
/// `/etc/profile.d/devc-features.sh` so they persist across sessions.
pub async fn install_features_via_exec(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    features: &[ResolvedFeature],
    remote_user: &str,
    progress: Option<&mpsc::UnboundedSender<String>>,
) -> Result<()> {
    let mut all_container_env: Vec<(String, String)> = Vec::new();

    for (i, feature) in features.iter().enumerate() {
        let short_name = feature
            .id
            .rsplit_once('/')
            .map(|(_, n)| n)
            .unwrap_or(&feature.id);

        if let Some(tx) = progress {
            let _ = tx.send(format!(
                "Installing feature {}/{}: {}...",
                i + 1,
                features.len(),
                short_name
            ));
        }

        // 1. Copy feature files into the container
        provider
            .copy_into(container_id, &feature.dir, "/tmp/dev-container-feature")
            .await?;

        // 2. Build environment variables
        let env = build_feature_env(&feature.options, remote_user);

        // 3. Run install.sh
        let exec_config = ExecConfig {
            cmd: vec![
                "sh".to_string(),
                "-c".to_string(),
                "chmod +x /tmp/dev-container-feature/install.sh \
                 && cd /tmp/dev-container-feature \
                 && /tmp/dev-container-feature/install.sh \
                 && rm -rf /tmp/dev-container-feature/"
                    .to_string(),
            ],
            env,
            user: Some("root".to_string()),
            ..Default::default()
        };

        let result = provider.exec(container_id, &exec_config).await?;

        if result.exit_code != 0 {
            return Err(crate::CoreError::ExecFailed(format!(
                "Feature install '{}' failed (exit code {}): {}",
                feature.id, result.exit_code, result.output
            )));
        }

        // 4. Collect containerEnv entries
        if let Some(ref container_env) = feature.metadata.container_env {
            for (key, value) in container_env {
                all_container_env.push((key.clone(), value.clone()));
            }
        }
    }

    // Write all containerEnv entries to a profile script
    if !all_container_env.is_empty() {
        let skipped = write_container_env(provider, container_id, &all_container_env).await?;
        if let Some(tx) = progress {
            for key in &skipped {
                let _ = tx.send(format!(
                    "Warning: skipped invalid containerEnv key {:?}",
                    key
                ));
            }
        }
    }

    Ok(())
}

/// Build the environment variable map for running a feature's install.sh.
///
/// This mirrors the env var construction in `dockerfile.rs:generate_feature_layer()`:
/// - Feature options are uppercased (e.g., `version` → `VERSION`)
/// - `_REMOTE_USER` and `_REMOTE_USER_HOME` are always set
pub fn build_feature_env(
    options: &HashMap<String, String>,
    remote_user: &str,
) -> HashMap<String, String> {
    let mut env = HashMap::new();

    for (key, value) in options {
        env.insert(key.to_uppercase(), value.clone());
    }

    let remote_user_home = if remote_user == "root" {
        "/root".to_string()
    } else {
        format!("/home/{}", remote_user)
    };

    env.insert("_REMOTE_USER".to_string(), remote_user.to_string());
    env.insert("_REMOTE_USER_HOME".to_string(), remote_user_home);

    env
}

/// Validate that an environment variable key is safe for shell export.
///
/// Accepts keys matching `^[a-zA-Z_][a-zA-Z0-9_]*$` — the POSIX standard
/// for environment variable names. Rejects keys that could cause shell injection.
fn is_valid_env_key(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    let mut chars = key.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Write containerEnv entries to `/etc/profile.d/devc-features.sh` so they
/// persist across shell sessions.
async fn write_container_env(
    provider: &dyn ContainerProvider,
    container_id: &ContainerId,
    env_entries: &[(String, String)],
) -> Result<Vec<String>> {
    let mut script = String::from("#!/bin/sh\n# Generated by devc - feature containerEnv\n");
    let mut skipped_keys = Vec::new();
    for (key, value) in env_entries {
        if !is_valid_env_key(key) {
            tracing::warn!("Skipping invalid env key: {:?}", key);
            skipped_keys.push(key.clone());
            continue;
        }
        // Use double quotes to allow variable expansion (e.g., $PATH)
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        script.push_str(&format!("export {}=\"{}\"\n", key, escaped));
    }

    let exec_config = ExecConfig {
        cmd: vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "mkdir -p /etc/profile.d && cat > /etc/profile.d/devc-features.sh << 'DEVC_EOF'\n{}\nDEVC_EOF\nchmod +x /etc/profile.d/devc-features.sh",
                script
            ),
        ],
        user: Some("root".to_string()),
        ..Default::default()
    };

    let mut last_output = String::new();
    for attempt in 0..3 {
        let result = provider.exec(container_id, &exec_config).await?;
        if result.exit_code == 0 {
            return Ok(skipped_keys);
        }
        last_output = result.output;
        tracing::warn!(
            "write_container_env exec failed (attempt {}/3): {}",
            attempt + 1,
            last_output.trim()
        );
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    Err(crate::CoreError::ExecFailed(format!(
        "Failed to write /etc/profile.d/devc-features.sh after 3 attempts: {}",
        last_output.trim()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::MockProvider;
    use devc_provider::ProviderType;

    // ==================== write_container_env tests ====================

    #[tokio::test]
    async fn test_write_container_env_returns_skipped_keys() {
        let provider = MockProvider::new(ProviderType::Docker);
        let container_id = ContainerId("test".to_string());
        let entries = vec![
            ("VALID_KEY".to_string(), "val1".to_string()),
            ("FOO;rm -rf /".to_string(), "evil".to_string()),
            ("ALSO_VALID".to_string(), "val2".to_string()),
            ("1BAD".to_string(), "nope".to_string()),
            ("".to_string(), "empty".to_string()),
        ];

        let skipped = write_container_env(&provider, &container_id, &entries)
            .await
            .unwrap();

        assert_eq!(skipped, vec!["FOO;rm -rf /", "1BAD", ""]);
    }

    #[tokio::test]
    async fn test_write_container_env_no_skipped_keys() {
        let provider = MockProvider::new(ProviderType::Docker);
        let container_id = ContainerId("test".to_string());
        let entries = vec![
            ("PATH".to_string(), "/usr/bin".to_string()),
            ("MY_VAR".to_string(), "hello".to_string()),
        ];

        let skipped = write_container_env(&provider, &container_id, &entries)
            .await
            .unwrap();

        assert!(skipped.is_empty());
    }

    // ==================== env key validation tests ====================

    #[test]
    fn test_validate_env_key_valid() {
        assert!(is_valid_env_key("PATH"));
        assert!(is_valid_env_key("MY_VAR"));
        assert!(is_valid_env_key("_REMOTE_USER"));
        assert!(is_valid_env_key("a"));
        assert!(is_valid_env_key("_"));
        assert!(is_valid_env_key("VAR123"));
    }

    #[test]
    fn test_validate_env_key_rejects_semicolons() {
        assert!(!is_valid_env_key("FOO;rm -rf /"));
    }

    #[test]
    fn test_validate_env_key_rejects_spaces() {
        assert!(!is_valid_env_key("FOO BAR"));
    }

    #[test]
    fn test_validate_env_key_rejects_equals() {
        assert!(!is_valid_env_key("FOO=bar"));
    }

    #[test]
    fn test_validate_env_key_rejects_empty() {
        assert!(!is_valid_env_key(""));
    }

    #[test]
    fn test_validate_env_key_rejects_leading_digit() {
        assert!(!is_valid_env_key("1FOO"));
    }

    // ==================== build_feature_env tests ====================

    #[test]
    fn test_build_feature_env_empty_options() {
        let env = build_feature_env(&HashMap::new(), "vscode");
        assert_eq!(env.get("_REMOTE_USER").unwrap(), "vscode");
        assert_eq!(env.get("_REMOTE_USER_HOME").unwrap(), "/home/vscode");
        assert_eq!(env.len(), 2);
    }

    #[test]
    fn test_build_feature_env_with_options() {
        let mut options = HashMap::new();
        options.insert("version".to_string(), "20".to_string());
        options.insert("nodeGypDependencies".to_string(), "true".to_string());

        let env = build_feature_env(&options, "dev");
        assert_eq!(env.get("VERSION").unwrap(), "20");
        assert_eq!(env.get("NODEGYPDEPENDENCIES").unwrap(), "true");
        assert_eq!(env.get("_REMOTE_USER").unwrap(), "dev");
        assert_eq!(env.get("_REMOTE_USER_HOME").unwrap(), "/home/dev");
    }

    #[test]
    fn test_build_feature_env_root_user() {
        let env = build_feature_env(&HashMap::new(), "root");
        assert_eq!(env.get("_REMOTE_USER").unwrap(), "root");
        assert_eq!(env.get("_REMOTE_USER_HOME").unwrap(), "/root");
    }
}
