//! Dockerfile snippet generation for devcontainer features

use super::resolve::ResolvedFeature;

/// Generate the Dockerfile layer for a single feature.
///
/// `build_dir_name` is the directory name under the build context where the
/// feature files have been copied (e.g., "feature-0-node").
///
/// `remote_user` is the container's remote user (e.g., "vscode").
pub fn generate_feature_layer(
    feature: &ResolvedFeature,
    build_dir_name: &str,
    remote_user: &str,
) -> String {
    let mut env_vars = String::new();

    // Add feature options as environment variables (uppercased keys)
    for (key, value) in &feature.options {
        let escaped = shell_escape(value);
        env_vars.push_str(&format!("{}={} ", key.to_uppercase(), escaped));
    }

    // Add special _REMOTE_USER vars
    let remote_user_home = if remote_user == "root" {
        "/root".to_string()
    } else {
        format!("/home/{}", remote_user)
    };

    env_vars.push_str(&format!(
        "_REMOTE_USER={} _REMOTE_USER_HOME={}",
        shell_escape(remote_user),
        shell_escape(&remote_user_home)
    ));

    let mut result = format!(
        "COPY {dir}/ /tmp/dev-container-feature/\n\
         RUN chmod +x /tmp/dev-container-feature/install.sh \\\n\
         \x20   && cd /tmp/dev-container-feature \\\n\
         \x20   && {env} /tmp/dev-container-feature/install.sh \\\n\
         \x20   && rm -rf /tmp/dev-container-feature/\n",
        dir = build_dir_name,
        env = env_vars.trim(),
    );

    // Add containerEnv as ENV instructions (makes tools available on PATH in all shells)
    // Values are emitted with double quotes to allow Docker variable expansion (e.g. $PATH)
    if let Some(ref container_env) = feature.metadata.container_env {
        for (key, value) in container_env {
            result.push_str(&format!("ENV {}=\"{}\"\n", key, value.replace('"', "\\\"")));
        }
    }

    result
}

/// Generate all feature layers for a Dockerfile.
///
/// Returns a string containing COPY+RUN blocks for each feature.
pub fn generate_all_feature_layers(
    features: &[ResolvedFeature],
    build_dir_prefix: &str,
    remote_user: &str,
) -> String {
    let mut layers = String::new();
    for (i, feature) in features.iter().enumerate() {
        let short_name = feature
            .id
            .rsplit_once('/')
            .map(|(_, n)| n)
            .unwrap_or(&feature.id)
            .replace(':', "-");
        let dir_name = format!("{}-{}-{}", build_dir_prefix, i, short_name);
        layers.push_str(&generate_feature_layer(feature, &dir_name, remote_user));
        layers.push('\n');
    }
    layers
}

/// Shell-escape a value for use in a Dockerfile RUN instruction.
///
/// Wraps in single quotes and escapes any internal single quotes.
fn shell_escape(value: &str) -> String {
    if value.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/') {
        // Safe to use unquoted
        value.to_string()
    } else {
        // Wrap in single quotes, escape internal single quotes
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::resolve::{FeatureMetadata, ResolvedFeature};
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn test_generate_feature_layer_basic() {
        let feature = ResolvedFeature {
            id: "ghcr.io/devcontainers/features/node:1".to_string(),
            dir: PathBuf::from("/tmp/cache/node"),
            options: HashMap::new(),
            metadata: FeatureMetadata::default(),
        };

        let layer = generate_feature_layer(&feature, "feature-0-node-1", "vscode");
        assert!(layer.contains("COPY feature-0-node-1/ /tmp/dev-container-feature/"));
        assert!(layer.contains("chmod +x /tmp/dev-container-feature/install.sh"));
        assert!(layer.contains("_REMOTE_USER=vscode"));
        assert!(layer.contains("_REMOTE_USER_HOME=/home/vscode"));
        assert!(layer.contains("rm -rf /tmp/dev-container-feature/"));
    }

    #[test]
    fn test_generate_feature_layer_with_options() {
        let mut options = HashMap::new();
        options.insert("version".to_string(), "18".to_string());

        let feature = ResolvedFeature {
            id: "ghcr.io/devcontainers/features/node:1".to_string(),
            dir: PathBuf::from("/tmp/cache/node"),
            options,
            metadata: FeatureMetadata::default(),
        };

        let layer = generate_feature_layer(&feature, "feature-0-node-1", "vscode");
        assert!(layer.contains("VERSION=18"));
        assert!(layer.contains("_REMOTE_USER=vscode"));
    }

    #[test]
    fn test_generate_feature_layer_root_user() {
        let feature = ResolvedFeature {
            id: "ghcr.io/devcontainers/features/git:1".to_string(),
            dir: PathBuf::from("/tmp/cache/git"),
            options: HashMap::new(),
            metadata: FeatureMetadata::default(),
        };

        let layer = generate_feature_layer(&feature, "feature-0-git-1", "root");
        assert!(layer.contains("_REMOTE_USER=root"));
        assert!(layer.contains("_REMOTE_USER_HOME=/root"));
    }

    #[test]
    fn test_generate_feature_layer_special_chars_in_value() {
        let mut options = HashMap::new();
        options.insert("packages".to_string(), "vim nano 'test'".to_string());

        let feature = ResolvedFeature {
            id: "feature".to_string(),
            dir: PathBuf::from("/tmp/cache/feature"),
            options,
            metadata: FeatureMetadata::default(),
        };

        let layer = generate_feature_layer(&feature, "feature-0", "vscode");
        // Should be shell-escaped
        assert!(layer.contains("PACKAGES="));
    }

    #[test]
    fn test_shell_escape_simple() {
        assert_eq!(shell_escape("18"), "18");
        assert_eq!(shell_escape("hello-world"), "hello-world");
        assert_eq!(shell_escape("v1.2.3"), "v1.2.3");
    }

    #[test]
    fn test_shell_escape_special() {
        assert_eq!(shell_escape("hello world"), "'hello world'");
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_generate_feature_layer_with_container_env() {
        let mut container_env = HashMap::new();
        container_env.insert("GOROOT".to_string(), "/usr/local/go".to_string());
        container_env.insert("PATH".to_string(), "/usr/local/go/bin:${PATH}".to_string());

        let feature = ResolvedFeature {
            id: "ghcr.io/devcontainers/features/go:1".to_string(),
            dir: PathBuf::from("/tmp/cache/go"),
            options: HashMap::new(),
            metadata: FeatureMetadata {
                container_env: Some(container_env),
                ..Default::default()
            },
        };

        let layer = generate_feature_layer(&feature, "feature-0-go-1", "vscode");
        // containerEnv should produce ENV instructions with double quotes (allowing $PATH expansion)
        assert!(layer.contains("ENV GOROOT=\"/usr/local/go\""));
        assert!(layer.contains("ENV PATH=\"/usr/local/go/bin:${PATH}\""));
        // Should NOT single-quote the value (that would prevent Docker expansion)
        assert!(!layer.contains("ENV PATH='"));
    }

    #[test]
    fn test_generate_all_feature_layers() {
        let features = vec![
            ResolvedFeature {
                id: "ghcr.io/devcontainers/features/git:1".to_string(),
                dir: PathBuf::from("/tmp/cache/git"),
                options: HashMap::new(),
                metadata: FeatureMetadata::default(),
            },
            ResolvedFeature {
                id: "ghcr.io/devcontainers/features/node:1".to_string(),
                dir: PathBuf::from("/tmp/cache/node"),
                options: {
                    let mut m = HashMap::new();
                    m.insert("version".to_string(), "20".to_string());
                    m
                },
                metadata: FeatureMetadata::default(),
            },
        ];

        let layers = generate_all_feature_layers(&features, "feature", "vscode");
        assert!(layers.contains("feature-0-git-1"));
        assert!(layers.contains("feature-1-node-1"));
        assert!(layers.contains("VERSION=20"));
    }
}
